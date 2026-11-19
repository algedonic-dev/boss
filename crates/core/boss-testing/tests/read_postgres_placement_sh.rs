//! The read-only ops verb `read-postgres-placement`
//! (`infra/forge/postgres-placement.sh <namespace>`; backlog eb7c4e0b).
//!
//! WHY. The review of the postgres-rollout forward fix (car 973c1488,
//! backlog 9123d811) needed the live placement of BOTH postgres
//! StatefulSets — whether either carries an affinity, nodeSelector or
//! tolerations key its rendering lacks, because that fix removes such a
//! key on its first converge and rolls the pod. dev-session is Forbidden a
//! StatefulSet get in boss-playground, so the playground stayed NOT
//! OBSERVED and no actor below David could read it. This verb reads it on
//! the forge, through the ops_kubectl door, and only reads.
//!
//! HOW THIS IS MEASURED. The script runs for real, with `sudo` stubbed on
//! PATH as a small cluster behind ops_kubectl's `docker run … kubectl
//! --kubeconfig=/kc` (the stand-in of read_longhorn_setting_sh.rs). The
//! live StatefulSet is a fixture; the RENDERING is not — it is
//! postgres-rollout.py's own `rendered_tree` over this checkout (and over
//! a generation copied from it, with no `.git`, as the forge runs verbs —
//! backlog 2cfea79b), so the rendered placement asserted here is whatever
//! the tree renders, read back off the verb's own lines rather than typed
//! into this file. The one thing the stand-in does in place of the cluster is
//! extract's client-side `create --dry-run=client` serialization, which
//! it answers with the real kubectl's `patch --local` (offline; the gate
//! image carries kubectl, infra/forge/boss-ci/Dockerfile). Every door
//! call lands in `argv`, so the tests can show the verb only read.
//! Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/postgres-placement.sh";
const VERB: &str = "read-postgres-placement";
const FIELDS: [&str; 3] = ["affinity", "nodeSelector", "tolerations"];

/// The stand-in behind the kubectl door. State under $STUB_STATE:
/// `live-<namespace>.json`, the StatefulSet a `get sts/postgres` answers
/// (absent: no bytes, as `--ignore-not-found` answers NotFound).
/// STUB_FORBIDDEN=1 answers every read as RBAC answered dev-session;
/// STUB_EMPTY=1 answers the live get with exit 0 and no bytes. A
/// client-side create is serialized offline by the real kubectl. Anything
/// else is exit 2, named.
const STUB: &str = r#"#!/bin/sh
case "$*" in
'-n docker image inspect '*|'-n docker create '*|'-n docker rm '*) exit 0 ;;
'-n docker container inspect '*) exit 1 ;;
esac
printf '%s\n' "$*" >> "$STUB_ARGV"
S="$STUB_STATE"
while [ $# -gt 0 ]; do
    case "$1" in --kubeconfig=/kc) shift; break ;; esac
    shift
done
if [ -n "${STUB_FORBIDDEN:-}" ]; then
    echo "Error from server (Forbidden): statefulsets.apps \"postgres\" is forbidden: User \"system:serviceaccount:boss-dev:dev-session\" cannot get resource \"statefulsets\" in API group \"apps\" in the namespace \"$4\"" >&2
    exit 1
fi
case "$1 $2" in
"get sts/postgres")
    [ -n "${STUB_EMPTY:-}" ] && exit 0
    [ -f "$S/live-$4.json" ] && cat "$S/live-$4.json"
    exit 0 ;;
"create --dry-run=client")
    exec env KUBECONFIG=/dev/null kubectl patch --local -f - --type=merge -p '{}' -o json ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

fn needs_tools() {
    for tool in ["jq", "kubectl", "python3", "git"] {
        let ok = Command::new("sh")
            .args(["-c", &format!("command -v {tool}")])
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "read_postgres_placement_sh: no {tool} on this box — the gate image has it, and a \
             test that cannot run must fail, never pass by returning early"
        );
    }
}

/// A live postgres StatefulSet in `ns` whose pod template carries
/// `placement` (a map of the three fields) and nothing else of note.
fn statefulset(ns: &str, placement: &Map<String, Value>) -> Value {
    let mut pod = json!({"containers": [{"name": "postgres", "image": "postgres:16"}]});
    for (k, v) in placement {
        pod[k] = v.clone();
    }
    json!({
        "apiVersion": "apps/v1", "kind": "StatefulSet",
        "metadata": {
            "name": "postgres", "namespace": ns,
            "uid": format!("{ns}-sts-uid"), "resourceVersion": "62678424", "generation": 7
        },
        "spec": {"replicas": 1, "template": {"spec": pod}},
        "status": {
            "observedGeneration": 6, "readyReplicas": 1, "updatedReplicas": 1,
            "currentRevision": "postgres-8494fb57c5", "updateRevision": "postgres-8494fb57c5"
        }
    })
}

struct Cluster {
    dir: PathBuf,
}

impl Cluster {
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        Cluster { dir }
    }

    fn live(&self, ns: &str, placement: &Map<String, Value>) {
        write_file(
            &self.dir.join("state").join(format!("live-{ns}.json")),
            &statefulset(ns, placement).to_string(),
        );
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.run_from(&repo_root(), args, env)
    }

    /// The verb started from the tree at `root` — this checkout, or a
    /// generation of root's tree (`generation` below).
    fn run_from(&self, root: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new("bash");
        c.arg(root.join(SCRIPT))
            .args(args)
            .env("PATH", path)
            .env_remove("BOSS_JOBS_URL")
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("TMPDIR", &self.dir)
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_STATE", self.dir.join("state"));
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().expect("the script runs")
    }

    /// Every door call: (whether the door attached stdin, the kubectl
    /// argv after the door's prefix).
    fn calls(&self) -> Vec<(bool, String)> {
        std::fs::read_to_string(self.dir.join("argv"))
            .unwrap_or_default()
            .lines()
            .map(|l| match l.split_once("--kubeconfig=/kc ") {
                Some((door, rest)) => (door.contains(" -i "), rest.to_string()),
                None => (false, l.to_string()),
            })
            .collect()
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn verdict(out: &str) -> Option<&str> {
    out.lines().rfind(|l| l.contains(": READ — "))
}

/// The rendered placement the verb printed, one `rendered <field>: …`
/// line per field — the tree's rendering, read back, never typed here.
fn rendered(out: &str) -> Map<String, Value> {
    let mut m = Map::new();
    for field in FIELDS {
        let prefix = format!("{VERB}: rendered {field}: ");
        let line = out
            .lines()
            .find_map(|l| l.strip_prefix(prefix.as_str()))
            .unwrap_or_else(|| panic!("the verb prints its rendered {field}:\n{out}"));
        if line != "absent" {
            m.insert(
                field.to_string(),
                serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}")),
            );
        }
    }
    m
}

fn keys(m: &Map<String, Value>) -> String {
    let k: Vec<&str> = FIELDS
        .iter()
        .copied()
        .filter(|f| m.contains_key(*f))
        .collect();
    if k.is_empty() {
        "none".into()
    } else {
        k.join(",")
    }
}

/// The verb read through the door and did nothing else: a `get` of the
/// one StatefulSet, or the rendering's client-side serialization over
/// stdin — never a server-side write of any kind.
fn only_read(c: &Cluster, ns: &str) {
    let calls = c.calls();
    assert!(!calls.is_empty(), "the verb read through the door");
    for (stdin, call) in &calls {
        if call.starts_with("get ") {
            assert_eq!(
                call,
                &format!(
                    "get sts/postgres -n {ns} --ignore-not-found -o json --request-timeout=30s"
                ),
                "the one get is the converge's own live read"
            );
            assert!(!stdin, "a get needs no stdin: {call}");
        } else {
            assert_eq!(
                call, "create --dry-run=client -f - -o json",
                "the only other call is extract's client-side serialization"
            );
            assert!(stdin, "the serialization reads the rendering on stdin");
        }
    }
}

// ------------------------------------------------------------------- read

#[test]
fn the_read_names_the_live_object_both_placements_and_what_a_converge_would_prune() {
    let c = Cluster::new("rpp-boss");

    // A live template with no placement at all: nothing to prune, and the
    // rendering is read back for the cases below.
    c.live("boss", &Map::new());
    let o = c.run(&["boss"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains(
            "read-postgres-placement: live sts/postgres in boss: uid boss-sts-uid, resourceVersion 62678424, generation 7, observedGeneration 6, currentRevision postgres-8494fb57c5, updateRevision postgres-8494fb57c5"
        ),
        "the identity, generation and revisions: {out}"
    );
    for field in FIELDS {
        assert!(
            out.contains(&format!("{VERB}: live {field}: absent")),
            "{field}: {out}"
        );
    }
    let want = rendered(&out);
    assert!(
        !want.is_empty(),
        "the tree pins postgres somewhere — a rendering with no placement would be news: {out}"
    );
    assert!(
        out.contains(&format!(
            "(the checkout {}",
            repo_root().canonicalize().expect("the repo").display()
        )) && out.contains(", through postgres-rollout.py rendered_tree)"),
        "{out}"
    );
    assert_eq!(
        verdict(&out),
        Some(
            format!(
                "{VERB}: READ — boss live=none rendered={} prune=none",
                keys(&want)
            )
            .as_str()
        ),
        "{out}"
    );
    only_read(&c, "boss");

    // Live exactly the rendering: nothing to prune.
    c.live("boss", &want);
    let o = c.run(&["boss"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let k = keys(&want);
    assert_eq!(
        verdict(&out),
        Some(format!("{VERB}: READ — boss live={k} rendered={k} prune=none").as_str()),
        "{out}"
    );
    assert!(!out.contains(": prune "), "{out}");

    // Live carries what the rendering lacks: a stale nodeSelector key, and
    // whole fields the rendering does not name. Each is named, at its depth.
    let (live, prune) = stale(&want);
    c.live("boss", &live);
    let o = c.run(&["boss"], &[]);
    let out = text(&o);
    assert!(
        o.status.success(),
        "a reading is an answer either way: {out}"
    );
    assert_eq!(
        verdict(&out),
        Some(
            format!(
                "{VERB}: READ — boss live=affinity,nodeSelector,tolerations rendered={k} prune={}",
                prune.join(",")
            )
            .as_str()
        ),
        "{out}"
    );
    for p in &prune {
        assert!(
            out.contains(&format!("{VERB}: prune {p}: live ")),
            "{p}: {out}"
        );
    }
    // The live fields are printed verbatim.
    for (field, value) in &live {
        assert!(
            out.contains(&format!("{VERB}: live {field}: {value}")),
            "{field} verbatim: {out}"
        );
    }
    only_read(&c, "boss");
}

/// `want` plus what the rendering lacks: every field it does not name,
/// and one nodeSelector key it cannot carry. Returns the live placement
/// and the prune list, as JSON pointers under spec.template.spec in the
/// order postgres-rollout.py's `changes` walks them (sorted keys).
fn stale(want: &Map<String, Value>) -> (Map<String, Value>, Vec<String>) {
    let mut live = want.clone();
    let mut prune = Vec::new();
    if !live.contains_key("affinity") {
        live.insert(
            "affinity".into(),
            json!({"nodeAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": {
                "nodeSelectorTerms": [{"matchExpressions": [
                    {"key": "boss.test/stale", "operator": "Exists"}]}]}}}),
        );
        prune.push("affinity".to_string());
    }
    match live.get_mut("nodeSelector") {
        Some(Value::Object(m)) => {
            m.insert("boss.test/stale".into(), json!("1"));
            prune.push("nodeSelector/boss.test~1stale".to_string());
        }
        _ => {
            live.insert("nodeSelector".into(), json!({"boss.test/stale": "1"}));
            prune.push("nodeSelector".to_string());
        }
    }
    if !live.contains_key("tolerations") {
        live.insert(
            "tolerations".into(),
            json!([{"key": "boss.test/stale", "operator": "Exists", "effect": "NoSchedule"}]),
        );
        prune.push("tolerations".to_string());
    }
    (live, prune)
}

#[test]
fn the_playground_is_read_and_rendered_as_its_own_namespace() {
    let c = Cluster::new("rpp-playground");
    c.live("boss-playground", &Map::new());
    let o = c.run(&["boss-playground"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let want = rendered(&out);
    let (live, prune) = stale(&want);
    c.live("boss-playground", &live);
    let o = c.run(&["boss-playground"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains("live sts/postgres in boss-playground: uid boss-playground-sts-uid"),
        "{out}"
    );
    assert_eq!(
        verdict(&out),
        Some(
            format!(
                "{VERB}: READ — boss-playground live=affinity,nodeSelector,tolerations rendered={} prune={}",
                keys(&want),
                prune.join(",")
            )
            .as_str()
        ),
        "{out}"
    );
    only_read(&c, "boss-playground");
}

/// The generation the first live runs met (ops-requests 4eff3ed4 and
/// 44e6bee1, backlog 2cfea79b): its directory name is the commit.
const GENERATION: &str = "3772da9caafa7d30901018538106ff3e04087c1d";

/// What the verb reads of its own tree: itself, the rollout it imports
/// and the claim reader that imports, the two libraries the shell
/// sources, and what render-instance.sh renders from.
const CLOSURE: &[&str] = &[
    SCRIPT,
    "infra/forge/postgres-placement.py",
    "infra/forge/postgres-rollout.py",
    "infra/forge/standalone-pvc.py",
    "infra/lib",
    "infra/estate",
    "infra/cluster",
    "examples",
];

/// A generation of root's tree on the forge (infra/forge/root-tree.sh), as
/// the ops runner starts a verb from it: `<root>/gen/<sha>/`, the commit's
/// tracked files with NO `.git`, saying which commit it is in
/// `.boss-tree-sha`. Built from this checkout's tracked files.
fn generation(c: &Cluster, sha: &str) -> PathBuf {
    let tree = c.dir.join("tree/gen").join(sha);
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root())
        .args(["ls-files", "-z", "--"])
        .args(CLOSURE)
        .output()
        .expect("git ls-files");
    assert!(out.status.success(), "{}", text(&out));
    let listed = String::from_utf8(out.stdout).expect("utf-8 paths");
    for path in listed.split('\0').filter(|p| !p.is_empty()) {
        let to = tree.join(path);
        std::fs::create_dir_all(to.parent().expect("a parent")).expect("mkdir");
        std::fs::copy(repo_root().join(path), &to).unwrap_or_else(|e| panic!("{path}: {e}"));
    }
    write_file(&tree.join(".boss-tree-sha"), &format!("{sha}\n"));
    assert!(!tree.join(".git").exists(), "a generation has no .git");
    tree
}

#[test]
fn the_verb_answers_from_a_generation_with_no_git_as_the_forge_runs_it() {
    let c = Cluster::new("rpp-generation");
    c.live("boss", &Map::new());

    // The checkout's rendering, to hold the generation's against.
    let o = c.run(&["boss"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let want = rendered(&out);

    // A marker that is not the generation it sits in names no commit: the
    // verb cannot say what it rendered, so it does not answer — before
    // any door opens.
    let tree = generation(&c, GENERATION);
    let marker = tree.join(".boss-tree-sha");
    let calls = c.calls().len();
    for wrong in ["0000000000000000000000000000000000000000", "", "HEAD"] {
        write_file(&marker, &format!("{wrong}\n"));
        let o = c.run_from(&tree, &["boss"], &[]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{wrong:?}: {out}");
        assert!(out.contains("CANNOT ANSWER"), "{wrong:?}: {out}");
        assert!(verdict(&out).is_none(), "{wrong:?}: {out}");
    }
    assert_eq!(c.calls().len(), calls, "no door opened");

    // The generation as root-tree.sh makes it: the verb renders the tree
    // itself and names the commit its marker and its directory both say.
    write_file(&marker, &format!("{GENERATION}\n"));
    let o = c.run_from(&tree, &["boss"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(!out.contains("not a git repository"), "{out}");
    assert_eq!(
        rendered(&out),
        want,
        "the generation renders as the checkout: {out}"
    );
    assert!(
        out.contains(&format!(
            "{VERB}: rendered from {GENERATION} (the generation {}, through",
            tree.canonicalize().expect("the generation").display()
        )),
        "names the generation's commit: {out}"
    );
    assert_eq!(
        verdict(&out),
        Some(
            format!(
                "{VERB}: READ — boss live=none rendered={} prune=none",
                keys(&want)
            )
            .as_str()
        ),
        "{out}"
    );
    only_read(&c, "boss");
}

#[test]
fn a_namespace_off_the_allow_list_is_refused_before_any_door_opens() {
    let c = Cluster::new("rpp-bounds");
    c.live("boss", &Map::new());
    for args in [
        vec![],
        vec!["boss-dev"],
        vec!["kube-system"],
        vec!["Boss"],
        vec!["boss "],
        vec!["-n"],
        vec!["boss", "extra"],
        vec!["--all"],
    ] {
        let o = c.run(&args, &[]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {out}");
        assert!(out.contains("REFUSED"), "{args:?}: {out}");
        assert!(verdict(&out).is_none(), "{args:?}: {out}");
    }
    assert!(c.calls().is_empty(), "no door opened: {:?}", c.calls());
}

#[test]
fn a_read_that_could_not_look_cannot_answer() {
    let c = Cluster::new("rpp-dark");
    c.live("boss-playground", &Map::new());

    // RBAC refuses the read, as it refused dev-session.
    let o = c.run(&["boss-playground"], &[("STUB_FORBIDDEN", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("CANNOT ANSWER") && out.contains("Forbidden"),
        "{out}"
    );
    assert!(verdict(&out).is_none(), "{out}");

    // An answer carrying no object — NotFound, or a door that said nothing
    // — is not an empty placement (d96e38ab).
    let o = c.run(&["boss-playground"], &[("STUB_EMPTY", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("CANNOT ANSWER") && out.contains("answered no object"),
        "{out}"
    );
    assert!(verdict(&out).is_none(), "{out}");
    let o = c.run(&["boss"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "no live-boss.json: {out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");

    // An object that is not the namespace's postgres StatefulSet.
    let mut wrong = statefulset("boss-playground", &Map::new());
    wrong["metadata"]["namespace"] = json!("boss");
    write_file(
        &c.dir.join("state/live-boss-playground.json"),
        &wrong.to_string(),
    );
    let o = c.run(&["boss-playground"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");
    assert!(verdict(&out).is_none(), "{out}");
    for (_, call) in c.calls() {
        assert!(call.starts_with("get sts/postgres "), "{call}");
    }
}

// ------------------------------------------------------------------ verb

/// The script's allow-list: its one `ALLOWED="…"` line, space-separated.
fn script_allow_list() -> Vec<String> {
    let src = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("the script");
    let line = src
        .lines()
        .find_map(|l| l.strip_prefix("ALLOWED=\""))
        .and_then(|l| l.strip_suffix('"'))
        .expect("the script carries one ALLOWED=\"…\" line");
    line.split_whitespace().map(str::to_string).collect()
}

#[test]
fn the_verb_file_offers_exactly_the_scripts_allow_list_and_reads_only() {
    let p = repo_root().join(format!("infra/ops/verbs/{VERB}.json"));
    let v: Value = serde_json::from_str(
        &std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())),
    )
    .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    assert_eq!(v["hosts"], json!(["forge"]), "the forge's ops_kubectl door");
    assert_eq!(v["argv"], json!([SCRIPT, "{1}"]));
    assert!(v["timeout"].as_u64().is_some(), "declares its timeout");
    let about = v["about"].as_str().unwrap_or_default();
    assert!(
        about.starts_with("READ-ONLY") && !about.contains("MUTATING"),
        "{about}"
    );
    assert!(v.get("requires_approval").is_none());

    // One fact, two files (CLAUDE.md §9a).
    let params = v["params"].as_array().expect("params");
    assert_eq!(params.len(), 1, "one param: {params:?}");
    assert_eq!(params[0]["name"], "namespace");
    assert!(params[0].get("pattern").is_none(), "one_of, not a shape");
    let offered: Vec<String> = params[0]["one_of"]
        .as_array()
        .expect("a one_of list")
        .iter()
        .map(|w| w.as_str().expect("a word").to_string())
        .collect();
    assert_eq!(
        offered,
        script_allow_list(),
        "the verb's one_of and the script's ALLOWED drifted"
    );
    assert_eq!(
        offered,
        ["boss", "boss-playground"],
        "the two instances whose postgres eb7c4e0b needs read"
    );
}

#[test]
fn the_reader_holds_no_write_and_renders_through_the_converges_own_code() {
    // The shell opens only the two door spellings and runs nothing
    // through them itself; the python reads through Recovery's `live` and
    // `rendered_tree` and never names a writing method — nor
    // `rendered_postgres`, whose git export cannot run in a generation
    // with no .git (backlog 2cfea79b).
    let sh = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("the script");
    let code: Vec<&str> = sh
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    assert!(
        code.iter()
            .all(|l| !l.contains("$K ") && !l.contains("apply") && !l.contains("patch")),
        "the shell runs no kubectl of its own"
    );
    let py = std::fs::read_to_string(repo_root().join("infra/forge/postgres-placement.py"))
        .expect("the reader");
    for uses in [
        "spec_from_file_location('postgres_rollout'",
        "recovery.live()",
        "recovery.rendered_tree(",
        "module.changes(",
    ] {
        assert!(py.contains(uses), "the reader uses {uses}");
    }
    for write in [
        ".patch(",
        ".rollback(",
        ".execute",
        ".save_ready(",
        ".journal_forward(",
        ".merged_spec(",
        ".placement(",
        ".wait(",
        "'apply'",
        "'delete'",
        ".rendered_postgres(",
    ] {
        assert!(!py.contains(write), "the reader never calls {write}");
    }
}
