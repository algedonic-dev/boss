//! `infra/forge/delete-orphan-object.sh` is RUN, not read — against a
//! stubbed `kubectl` and a fixture tree in root's tree's own shape (a
//! bare store and a generation exported from it, `export_generation`),
//! so every verdict below is one the script actually reached.
//!
//! WHY THE VERB EXISTS (backlog a139d5bb, filed urgent 2026-09-10). The
//! converge runs `kubectl apply` with no `--prune`, so deleting a
//! manifest removes the DECLARATION and leaves the OBJECT running. The
//! only way to clear one was a human typing `kubectl -n boss delete svc
//! <name>`, and `infra/ops/verbs/` — the allowlist the ops-runner
//! executes — had no verb that could. A gated-green car whose lint
//! exits 1 on an orphan therefore could not land: the next converge
//! would fail until a person intervened.
//!
//! WHY NOT A `kubectl delete` VERB. The allowlist's whole value is that
//! every verb is a reviewed word with bounded params; "delete any object
//! by name" hands an agent namespace-wide destruction through an audited
//! door, which is worse than no door. So the verb's authority is
//! DERIVED, not granted: `infra/cluster/undeclared-objects.sh` re-runs
//! the same computation the orphan lint uses — every live object in the
//! namespaces the tree owns that no manifest declares — and the verb
//! acts on one named object ONLY IF that computation names it. Every
//! other answer is a refusal that says which test refused.
//!
//! What each case below pins, in the order the script applies them:
//! the param shape, a declaration set that is a verified generation of
//! root's tree (backlog 1dde630c), the derivation's verdict
//! (declared / out of scope / not live / controller-owned / unreadable),
//! the verb's own kind floor, and the capture-before-delete. Plus the
//! allowlist's own validation, exercised THROUGH `ops-runner.sh` with
//! the real verb files.
//!
//! Nothing here touches a cluster. `kubectl` is a stub on every path,
//! and a `delete` it receives is appended to a file.

use boss_testing::{repo_root, write_exec};
use std::path::{Path, PathBuf};
use std::process::Command;

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// A scratch root PER PROCESS, not per case name. A fixed
/// `/tmp/delete-orphan-object-<case>` is shared by every account on the
/// box: a dir left by a root run is one `remove_dir_all` a later run as
/// another uid cannot do — and that failure was discarded, so the run
/// carried on and died 130 lines later on an unreadable bare
/// `PermissionDenied` from a fixture write. `scratch` carries the uid
/// and the pid, so the root is ours — the pid alone left a recycled
/// pid's leftover from another uid in reach (307df975) — and every
/// failure here names the path it was at.
fn scratch(case: &str) -> PathBuf {
    boss_testing::scratch_dir(&format!("delete-orphan-object-{case}"))
}

/// Write, naming the path when it fails. A bare `unwrap()` on a write
/// reports the errno and not the file, which is the whole diagnosis.
fn write_file(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A manifest "file" holding one or more objects. The fixture writes
/// JSON into a `.yaml` file — JSON is valid YAML, and `kubectl create
/// --dry-run -o json -f` echoes it back, so the stub can simply `cat`.
fn doc(kind: &str, ns: &str, name: &str) -> String {
    if ns.is_empty() {
        format!(r#"{{"kind":"{kind}","metadata":{{"name":"{name}"}}}}"#)
    } else {
        format!(r#"{{"kind":"{kind}","metadata":{{"name":"{name}","namespace":"{ns}"}}}}"#)
    }
}

/// `git <args>` in `dir`, its trimmed stdout — failing by name.
fn git_out(dir: &Path, args: &[&str], env: &[(&str, &Path)]) -> String {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("chmod {mode:o} {}: {e}", path.display()));
}

/// ROOT'S TREE, in the shape `infra/forge/root-tree.sh` keeps it
/// (backlog 1dde630c): the checkout `work`'s HEAD fetched into the bare
/// store `rt/repo.git`, exported whole as `rt/gen/<sha>` — read-tree and
/// checkout-index, the export's own two commands — and stamped with
/// `.boss-tree-sha`. Returns the generation, which is what the verb is
/// pointed at. Each directory is 0755, as root-tree.sh's umask makes them,
/// so the run's own umask cannot decide a write-permission verdict.
fn export_generation(work: &Path) -> PathBuf {
    let rt = work.parent().unwrap().join("rt");
    let store = rt.join("repo.git");
    if !store.exists() {
        std::fs::create_dir_all(&rt).unwrap();
        git(&rt, &["init", "-q", "--bare", "repo.git"]);
    }
    git(
        work,
        &[
            "push",
            "-q",
            store.to_str().unwrap(),
            "HEAD:refs/boss/accepted",
        ],
    );
    let sha = git_out(work, &["rev-parse", "HEAD"], &[]);
    let generation = rt.join("gen").join(&sha);
    std::fs::create_dir_all(&generation).unwrap();
    let index = rt.join(format!(".{sha}.idx"));
    let git_dir = format!("--git-dir={}", store.display());
    let work_tree = format!("--work-tree={}", generation.display());
    for args in [
        vec![
            git_dir.as_str(),
            work_tree.as_str(),
            "read-tree",
            sha.as_str(),
        ],
        vec![
            git_dir.as_str(),
            work_tree.as_str(),
            "checkout-index",
            "-a",
            "-f",
        ],
    ] {
        git_out(&rt, &args, &[("GIT_INDEX_FILE", &index)]);
    }
    std::fs::remove_file(&index).unwrap();
    write_file(&generation.join(".boss-tree-sha"), &format!("{sha}\n"));
    for d in [&rt, &rt.join("gen"), &generation, &store] {
        chmod(d, 0o755);
    }
    generation
}

/// The checkout the fixture commits in: a real git repository laid out
/// like the repo, with enough manifests to clear the derivation's "the
/// scrape broke" floors (at least 10 files, at least 20 declared
/// objects). The verb never reads it — it reads the generation
/// `export_generation` makes of it.
fn fixture_checkout(root: &Path) -> PathBuf {
    let tree = root.join("work");
    let dir = tree.join("infra/cluster/manifests");
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("fixture manifests dir {}: {e}", dir.display()));
    std::fs::create_dir_all(tree.join("infra/gate-runner"))
        .unwrap_or_else(|e| panic!("fixture gate-runner dir under {}: {e}", tree.display()));

    let mut files: Vec<(String, String)> = vec![
        (
            "namespaces.yaml".into(),
            format!(
                "{}\n{}\n",
                doc("Namespace", "", "boss"),
                doc("Namespace", "", "boss-dev")
            ),
        ),
        (
            "cluster-scoped.yaml".into(),
            format!(
                "{}\n{}\n{}\n",
                doc("ClusterRole", "", "boss-reader"),
                doc("ClusterRoleBinding", "", "boss-reader"),
                doc("StorageClass", "", "longhorn-r1"),
            ),
        ),
        (
            "configmaps.yaml".into(),
            format!("{}\n", doc("ConfigMap", "boss", "boss-config")),
        ),
        (
            "deployment.yaml".into(),
            format!("{}\n", doc("Deployment", "boss", "boss")),
        ),
        (
            "cronjob.yaml".into(),
            format!("{}\n", doc("CronJob", "boss", "boss-backup")),
        ),
        (
            "statefulset.yaml".into(),
            r#"{"kind":"StatefulSet","metadata":{"name":"postgres","namespace":"boss"},
 "spec":{"volumeClaimTemplates":[{"metadata":{"name":"pgdata"}}]}}
"#
            .into(),
        ),
        (
            "dev-access.yaml".into(),
            format!(
                "{}\n{}\n{}\n",
                doc("Role", "boss-dev", "dev-session"),
                doc("RoleBinding", "boss-dev", "dev-session"),
                doc("Secret", "boss-dev", "dev-session-token"),
            ),
        ),
        (
            "dev.yaml".into(),
            format!(
                "{}\n{}\n",
                doc("Service", "boss-dev", "boss-dev"),
                doc("Deployment", "boss-dev", "boss-dev"),
            ),
        ),
        (
            "serviceaccount.yaml".into(),
            format!("{}\n", doc("ServiceAccount", "boss", "boss")),
        ),
        (
            "pvc.yaml".into(),
            format!("{}\n", doc("PersistentVolumeClaim", "boss", "boss-auth")),
        ),
    ];
    // Ten Services in `boss`, one file each: enough files and objects
    // that the floors are cleared by the fixture rather than relaxed
    // for it.
    for n in [
        "boss-jobs-internal",
        "boss-nats-internal",
        "boss-clock-internal",
        "boss-dispatcher-internal",
        "boss-ledger-internal",
        "boss-people-internal",
        "boss-events-internal",
        "boss-policy-internal",
        "boss-content-internal",
        "boss-front",
    ] {
        files.push((
            format!("{n}.yaml"),
            format!("{}\n", doc("Service", "boss", n)),
        ));
    }
    for (name, body) in &files {
        write_file(&dir.join(name), body);
    }

    git(&tree, &["init", "-q", "-b", "main"]);
    git(&tree, &["add", "-A"]);
    git(
        &tree,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture manifests",
        ],
    );
    tree
}

/// The stubbed `kubectl`. It answers exactly the six invocation shapes
/// the derivation and the verb use, reading the "cluster" from files:
///
///   $STUB_LIVE/<Kind>.<ns>  one line per live object: name<TAB>ownerKind
///   $STUB_FORBID            "<Kind> <ns>" per line — not listable
///   $STUB_BADPARSE          a manifest basename that fails to parse
///   $STUB_DELETED           every delete it is asked for, appended
fn stub_kubectl(root: &Path) -> PathBuf {
    let path = root.join("kubectl-stub");
    write_exec(
        &path,
        r#"#!/bin/bash
set -u
live() { # kind ns
    f="$STUB_LIVE/$1.$2"
    [ -f "$f" ] && cat "$f"
}
forbidden() { # kind ns
    [ -n "${STUB_FORBID:-}" ] && [ -f "$STUB_FORBID" ] && grep -qxF "$1 $2" "$STUB_FORBID"
}
case "${1:-}" in
version)
    echo '{}' ;;
create)
    f=""
    while [ $# -gt 0 ]; do [ "$1" = "-f" ] && { f="$2"; break; }; shift; done
    [ -n "$f" ] || { echo "stub: no -f" >&2; exit 1; }
    if [ -n "${STUB_BADPARSE:-}" ] && [ "$(basename "$f")" = "$STUB_BADPARSE" ]; then
        echo "error: error parsing $f: broken fixture" >&2
        exit 1
    fi
    cat "$f" ;;
get)
    kind="$2"; shift 2
    name=""
    case "${1:-}" in -*|"") ;; *) name="$1"; shift ;; esac
    ns=""; fmt=""
    while [ $# -gt 0 ]; do
        case "$1" in
            -n) ns="$2"; shift 2 ;;
            -o) fmt="$2"; shift 2 ;;
            *) shift ;;
        esac
    done
    forbidden "$kind" "$ns" && { echo "Error from server (Forbidden): $kind is forbidden" >&2; exit 1; }
    # The read-back after a delete, when the case says the API server
    # stopped answering: a failure that is NOT NotFound.
    if [ -n "$name" ] && [ -s "$STUB_DELETED" ] && [ "${STUB_AFTER_DELETE:-}" = unreachable ]; then
        echo "Unable to connect to the server: dial tcp 10.0.0.1:6443: i/o timeout" >&2
        exit 1
    fi
    # ...or the server answered, but not NotFound: the credential lost
    # the get between the delete and the read.
    if [ -n "$name" ] && [ -s "$STUB_DELETED" ] && [ "${STUB_AFTER_DELETE:-}" = forbidden ]; then
        echo "Error from server (Forbidden): $kind \"$name\" is forbidden: User \"system:serviceaccount:boss:ops\" cannot get resource" >&2
        exit 1
    fi
    if [ -z "$name" ]; then
        live "$kind" "$ns"
        exit 0
    fi
    row=$(live "$kind" "$ns" | awk -F'\t' -v n="$name" '$1 == n')
    if [ -z "$row" ]; then
        echo "Error from server (NotFound): $kind \"$name\" not found" >&2
        exit 1
    fi
    case "$fmt" in
        yaml) printf 'kind: %s\nmetadata:\n  name: %s\n  namespace: %s\n' "$kind" "$name" "$ns" ;;
        *) printf '%s\n' "$row" | cut -f2 ;;
    esac ;;
delete)
    kind="$2"; name="$3"; shift 3
    ns=""
    while [ $# -gt 0 ]; do case "$1" in -n) ns="$2"; shift 2 ;; *) shift ;; esac; done
    printf '%s\t%s\t%s\n' "$kind" "$ns" "$name" >> "$STUB_DELETED"
    f="$STUB_LIVE/$kind.$ns"
    [ "${STUB_AFTER_DELETE:-}" = kept ] && f=/nonexistent
    [ -f "$f" ] && { grep -v "^$name	" "$f" > "$f.new" || true; mv "$f.new" "$f"; }
    echo "$kind \"$name\" deleted" ;;
*)
    echo "stub kubectl: unexpected invocation: $*" >&2
    exit 64 ;;
esac
"#,
    );
    path
}

/// The live "cluster": `Kind.ns` files of `name<TAB>ownerKind` rows.
fn live_set(root: &Path, rows: &[(&str, &str, &str, &str)]) -> PathBuf {
    let dir = root.join("live");
    std::fs::create_dir_all(&dir).unwrap();
    for (kind, ns, name, owner) in rows {
        let f = dir.join(format!("{kind}.{ns}"));
        let mut body = std::fs::read_to_string(&f).unwrap_or_default();
        body.push_str(&format!("{name}\t{owner}\n"));
        std::fs::write(&f, body).unwrap();
    }
    dir
}

/// Every object the fixture tree declares, live — plus whatever the
/// case adds. A realistic cluster agrees with the tree except for the
/// orphans under test.
fn declared_live() -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    let mut v = vec![
        ("ConfigMap", "boss", "boss-config", ""),
        ("Deployment", "boss", "boss", ""),
        ("CronJob", "boss", "boss-backup", ""),
        ("StatefulSet", "boss", "postgres", ""),
        ("ServiceAccount", "boss", "boss", ""),
        ("PersistentVolumeClaim", "boss", "boss-auth", ""),
        ("Role", "boss-dev", "dev-session", ""),
        ("RoleBinding", "boss-dev", "dev-session", ""),
        ("Service", "boss-dev", "boss-dev", ""),
        ("Deployment", "boss-dev", "boss-dev", ""),
    ];
    for n in [
        "boss-jobs-internal",
        "boss-nats-internal",
        "boss-clock-internal",
        "boss-dispatcher-internal",
        "boss-ledger-internal",
        "boss-people-internal",
        "boss-events-internal",
        "boss-policy-internal",
        "boss-content-internal",
        "boss-front",
    ] {
        v.push(("Service", "boss", n, ""));
    }
    v
}

struct Case {
    root: PathBuf,
    /// The checkout the fixture commits in — never what the verb reads.
    work: PathBuf,
    /// The generation of root's tree exported from `work`'s HEAD.
    tree: PathBuf,
    kubectl: PathBuf,
    live: PathBuf,
    deleted: PathBuf,
}

impl Case {
    /// A case with the fixture tree, the stub, and a live set that is
    /// the declared set plus `extra`.
    fn new(name: &str, extra: &[(&str, &str, &str, &str)]) -> Self {
        let root = scratch(name);
        let work = fixture_checkout(&root);
        let tree = export_generation(&work);
        let kubectl = stub_kubectl(&root);
        let mut rows = declared_live();
        rows.extend_from_slice(extra);
        let live = live_set(&root, &rows);
        let deleted = root.join("deleted");
        std::fs::write(&deleted, "").unwrap();
        Case {
            root,
            work,
            tree,
            kubectl,
            live,
            deleted,
        }
    }

    /// Commit what the case wrote into `work`, and export that commit as
    /// the generation the verb reads — the way main reaches root's tree.
    fn commit(&mut self, message: &str) {
        git(&self.work, &["add", "-A"]);
        git(
            &self.work,
            &["-c", "commit.gpgsign=false", "commit", "-qm", message],
        );
        self.tree = export_generation(&self.work);
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join("infra/forge/delete-orphan-object.sh"))
            .args(args)
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("BOSS_KUBECTL", &self.kubectl)
            .env("BOSS_CLUSTER_TREE", &self.tree)
            .env("STUB_LIVE", &self.live)
            .env("STUB_DELETED", &self.deleted)
            .env("STUB_FORBID", self.root.join("forbid"));
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("delete-orphan-object.sh runs");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    fn deletions(&self) -> String {
        std::fs::read_to_string(&self.deleted).unwrap_or_default()
    }
}

fn contains_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(
            text.contains(n),
            "{what}: output does not name {n:?}:\n{text}"
        );
    }
}

/// Run the actual readers from the proved generation, not the test checkout.
/// Otherwise importlib's bytecode writes miss the tree whose cleanliness is
/// being judged, and a first dry-run silently consumes that generation.
fn self_contained_generation(case: &mut Case) {
    for relative in [
        "infra/forge/delete-orphan-object.sh",
        "infra/cluster/undeclared-objects.sh",
        "infra/dev/dev-declaration.py",
        "infra/dev/dev-build.py",
        "infra/forge/isolation-fixture-declaration.py",
        "infra/forge/isolation-fixture.py",
    ] {
        let target = case.work.join(relative);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(repo_root().join(relative), target).unwrap();
    }
    case.commit("include the actual generation programs and declaration readers");
}

fn run_from_generation(case: &Case, relative: &str, args: &[&str]) -> (i32, String) {
    let out = Command::new("bash")
        .arg(case.tree.join(relative))
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_KUBECTL", &case.kubectl)
        .env("STUB_LIVE", &case.live)
        .env("STUB_DELETED", &case.deleted)
        .env("STUB_FORBID", case.root.join("forbid"))
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Exercise the flags actually selected by every shell entry into these
/// importlib programs, including the pure validator and converge renderer.
fn generation_python_entries() -> Vec<(String, Vec<String>, String)> {
    let mut entries = Vec::new();
    for shell in std::fs::read_dir(repo_root().join("infra/forge")).unwrap() {
        let shell = shell.unwrap().path();
        if shell.extension().and_then(|ext| ext.to_str()) != Some("sh") {
            continue;
        }
        for line in std::fs::read_to_string(&shell).unwrap().lines() {
            let words: Vec<_> = line.split_whitespace().collect();
            if words.first() != Some(&"python3") {
                continue;
            }
            for program in ["isolation-fixture-host.py", "dev-work-volume.py"] {
                if let Some(index) = words.iter().position(|word| word.contains(program)) {
                    entries.push((
                        shell.file_name().unwrap().to_string_lossy().into_owned(),
                        words[1..index]
                            .iter()
                            .map(|word| (*word).to_owned())
                            .collect(),
                        program.to_owned(),
                    ));
                }
            }
        }
    }
    entries.sort();
    entries
}

#[test]
fn every_root_generation_python_entry_keeps_later_root_verbs_usable() {
    let mut case = Case::new(
        "all-python-entries",
        &[("ConfigMap", "boss", "orphan-x", "")],
    );
    self_contained_generation(&mut case);
    for program in [
        "isolation-fixture-host.py",
        "isolation-fixture-port.py",
        "dev-work-volume.py",
        "dev-work-evidence.py",
    ] {
        let relative = format!("infra/forge/{program}");
        std::fs::copy(repo_root().join(&relative), case.work.join(relative)).unwrap();
    }
    case.commit("include real Python adapters in the verified generation");
    let entries = generation_python_entries();
    assert!(
        entries.len() >= 4,
        "validator, host, volume and converge entries must run"
    );
    for (shell, flags, program) in entries {
        let mut command = Command::new("python3");
        command
            .args(&flags)
            .arg(case.tree.join("infra/forge").join(&program));
        if program == "isolation-fixture-host.py" {
            command.args([
                "validate",
                "observe",
                "11111111-1111-4111-8111-111111111111",
                "22222222-2222-4222-8222-222222222222",
            ]);
        } else {
            // The fixed false native port refuses before any observation/effect,
            // but after the real evidence carrier has been imported.
            command.args(["--plan", "50Gi"]);
        }
        command
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("BOSS_DEV_WORK_KUBECTL", "false");
        if program == "isolation-fixture-host.py" {
            // -I ignores this environment setting: the entry needs -B.
            command.env("PYTHONDONTWRITEBYTECODE", "1");
        }
        let output = command.output().unwrap();
        println!(
            "{shell} {flags:?} {program}: {:?}\n{}{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if program == "isolation-fixture-host.py" {
            assert!(output.status.success(), "pure validator must execute");
        } else {
            assert!(!output.status.success(), "unread native port must refuse");
            assert!(String::from_utf8_lossy(&output.stderr).contains("REFUSED"));
        }
        let dry_run = run_from_generation(
            &case,
            "infra/forge/delete-orphan-object.sh",
            &["ConfigMap/boss/orphan-x", "--dry-run"],
        );
        assert_eq!(
            dry_run.0, 0,
            "{shell} {flags:?} polluted the proved generation: {}",
            dry_run.1
        );
        assert!(!case.tree.join("infra/forge/__pycache__").exists());
        assert!(case.deletions().is_empty(), "fixtures must never delete");
    }
}

#[test]
fn root_generation_python_entry_class_explicitly_disables_bytecode() {
    // Environment suppression alone is ineffective under Python isolation (-I).
    // Read every shell invocation of the importing programs, rather than pinning
    // four line numbers and silently missing a new entry into the same class.
    let entries = generation_python_entries();
    assert!(entries.len() >= 4);
    for (shell, flags, program) in entries {
        assert!(
            flags.iter().any(|flag| flag == "-B"),
            "{shell}: {program} must disable generation writes explicitly; flags={flags:?}"
        );
    }
}

#[test]
fn root_generation_dry_run_can_run_twice_without_polluting_it() {
    let mut case = Case::new(
        "self-contained-twice",
        &[("ConfigMap", "boss", "orphan-x", "")],
    );
    self_contained_generation(&mut case);
    let args = ["ConfigMap/boss/orphan-x", "--dry-run"];
    let first = run_from_generation(&case, "infra/forge/delete-orphan-object.sh", &args);
    let second = run_from_generation(&case, "infra/forge/delete-orphan-object.sh", &args);
    println!("first generation run: {first:?}\nsecond generation run: {second:?}");
    assert_eq!(first.0, 0, "first generation run: {}", first.1);
    assert_eq!(second.0, 0, "second generation run: {}", second.1);
    for directory in ["infra/dev/__pycache__", "infra/forge/__pycache__"] {
        assert!(
            !case.tree.join(directory).exists(),
            "reader wrote {directory}"
        );
    }
    assert!(case.deletions().is_empty(), "dry-runs must not delete");
}

#[test]
fn root_generation_other_declaration_callers_keep_the_delete_verb_usable() {
    let mut case = Case::new(
        "self-contained-declaration",
        &[("ConfigMap", "boss", "orphan-x", "")],
    );
    self_contained_generation(&mut case);
    let declared = run_from_generation(
        &case,
        "infra/cluster/undeclared-objects.sh",
        &["--declared"],
    );
    let delete = run_from_generation(
        &case,
        "infra/forge/delete-orphan-object.sh",
        &["ConfigMap/boss/orphan-x", "--dry-run"],
    );
    println!("direct declaration: {declared:?}\nsubsequent dry-run: {delete:?}");
    assert_eq!(declared.0, 0, "direct declaration: {}", declared.1);
    assert_eq!(delete.0, 0, "subsequent dry-run: {}", delete.1);
    assert!(
        case.deletions().is_empty(),
        "declaration and dry-run must not delete"
    );
}

#[test]
fn root_generation_refusal_indents_every_unrecorded_path() {
    let case = Case::new("two-unrecorded-paths", &[]);
    write_file(&case.tree.join("a-cache.pyc"), "unrecorded");
    write_file(&case.tree.join("b-cache.pyc"), "unrecorded");
    let (status, output) = case.run(&["ConfigMap/boss/orphan-x", "--dry-run"]);
    assert_eq!(status, 2, "{output}");
    contains_all(
        &output,
        &["\n    a-cache.pyc\n", "\n    b-cache.pyc\n"],
        "unrecorded paths",
    );
}

// ---------------------------------------------------------------------------
// The one object the verb exists for.
// ---------------------------------------------------------------------------

/// A live object in a namespace the tree owns, of a kind the tree
/// declares there, that no manifest declares: the derivation names it,
/// so the verb deletes it — and captures what it was first, because a
/// deleted object cannot be read back.
#[test]
fn deletes_the_one_object_the_tree_proves_undeclared() {
    let c = Case::new("orphan", &[("Service", "boss", "boss-docs-internal", "")]);
    let (rc, out) = c.run(&["Service/boss/boss-docs-internal"]);
    assert_eq!(rc, 0, "the verb refused a genuine orphan:\n{out}");
    assert_eq!(
        c.deletions().trim(),
        "Service\tboss\tboss-docs-internal",
        "the verb did not delete exactly the named object:\n{out}"
    );
    contains_all(
        &out,
        &[
            "undeclared",
            "boss-docs-internal",
            "kind: Service",
            "is gone from `boss` (read back: NotFound)",
        ],
        "the orphan case",
    );
    // The commit the authority came from is the generation's own.
    let sha = c.tree.file_name().unwrap().to_string_lossy().to_string();
    contains_all(
        &out,
        &[&format!(
            "undeclared by infra/cluster/manifests at {}",
            &sha[..12]
        )],
        "the orphan case's commit",
    );
}

/// GONE IS NotFound, NOT "THE GET FAILED" (backlog 1058e686, car B). The
/// read-back after the delete used to count ANY failed get as gone, so
/// an API server that stopped answering between the delete and the read
/// printed OK. Now the delete has happened, nothing showed the object
/// gone, and the run says exactly that and fails.
#[test]
fn a_read_back_that_is_not_notfound_is_not_gone() {
    let c = Case::new(
        "unproven-gone",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    let (rc, out) = c.run_env(
        &["Service/boss/boss-docs-internal"],
        &[("STUB_AFTER_DELETE", "unreachable".to_string())],
    );
    assert_eq!(rc, 1, "an unanswered read-back passed:\n{out}");
    assert_eq!(
        c.deletions().trim(),
        "Service\tboss\tboss-docs-internal",
        "the delete itself still ran:\n{out}"
    );
    contains_all(
        &out,
        &["CANNOT ANSWER — deleted, not proven gone", "i/o timeout"],
        "the unproven case",
    );
    assert!(!out.contains(": OK — "), "no OK line:\n{out}");
}

/// The SERVER answering is not the server answering NotFound. An auth
/// error after the delete is also "Error from server", so a read-back
/// loosened to that prefix would print OK here — the mutation car B's
/// review ran, which every other case survived (backlog 1058e686,
/// note_2026_09_29_timeouts).
#[test]
fn an_auth_error_on_the_read_back_is_not_gone() {
    let c = Case::new(
        "forbidden-read-back",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    let (rc, out) = c.run_env(
        &["Service/boss/boss-docs-internal"],
        &[("STUB_AFTER_DELETE", "forbidden".to_string())],
    );
    assert_eq!(rc, 1, "a forbidden read-back passed:\n{out}");
    contains_all(
        &out,
        &[
            "CANNOT ANSWER — deleted, not proven gone",
            "Error from server (Forbidden)",
        ],
        "the forbidden case",
    );
    assert!(!out.contains(": OK — "), "no OK line:\n{out}");
}

/// A delete that answered success over an object that is still there is
/// named, not read as gone.
#[test]
fn an_object_still_there_after_the_delete_fails_by_name() {
    let c = Case::new(
        "still-there",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    let (rc, out) = c.run_env(
        &["Service/boss/boss-docs-internal"],
        &[("STUB_AFTER_DELETE", "kept".to_string())],
    );
    assert_eq!(rc, 1, "{out}");
    contains_all(&out, &["is STILL THERE"], "the still-there case");
    assert!(!out.contains(": OK — "), "no OK line:\n{out}");
}

/// `--dry-run` reaches the same verdict and deletes nothing: the way to
/// ask the audited door "would this be allowed?" without acting.
#[test]
fn dry_run_answers_without_deleting() {
    let c = Case::new("dry-run", &[("Service", "boss", "boss-docs-internal", "")]);
    let (rc, out) = c.run(&["Service/boss/boss-docs-internal", "--dry-run"]);
    assert_eq!(rc, 0, "--dry-run refused a genuine orphan:\n{out}");
    assert_eq!(c.deletions(), "", "--dry-run deleted something:\n{out}");
    contains_all(&out, &["DRY RUN", "boss-docs-internal"], "the dry-run case");
}

// ---------------------------------------------------------------------------
// Every refusal names what refused it.
// ---------------------------------------------------------------------------

/// The object the tree DOES declare. The refusal names the object and
/// the manifest that declares it — a verdict nobody has to re-derive.
#[test]
fn refuses_an_object_the_tree_declares() {
    let c = Case::new("declared", &[]);
    let (rc, out) = c.run(&["Service/boss/boss-jobs-internal"]);
    assert_ne!(rc, 0, "the verb accepted a DECLARED object:\n{out}");
    assert_eq!(c.deletions(), "", "a declared object was deleted:\n{out}");
    contains_all(
        &out,
        &["boss-jobs-internal", "declared", "boss-jobs-internal.yaml"],
        "the declared case",
    );
}

const RENDERER_REL: &str = "infra/estate/render-dev-door-config.sh";

/// The shipped renderer's text and the ConfigMap name its one
/// `RENDERS="ConfigMap/<name>"` line declares — read, not typed here.
fn shipped_renderer() -> (String, String) {
    let renderer = std::fs::read_to_string(repo_root().join(RENDERER_REL)).expect("the renderer");
    let name = renderer
        .lines()
        .find_map(|l| l.strip_prefix("RENDERS=\"ConfigMap/"))
        .and_then(|l| l.strip_suffix('"'))
        .unwrap_or_else(|| panic!("{RENDERER_REL} names no `RENDERS=\"ConfigMap/<name>\"`"))
        .to_string();
    (renderer, name)
}

/// One instance, `prod` in namespace `boss`, and the renderer beside it:
/// together they exempt the object the converge renders per instance.
fn write_instance_and_renderer(tree: &Path, renderer_body: &str) {
    write_file(
        &tree.join("infra/cluster/instances.toml"),
        "source = \"prod\"\n\n[prod]\nnamespace = \"boss\"\ntenant_dir = \"examples/brewery\"\nsim = false\nhostname = \"h.example\"\nguest = false\n",
    );
    std::fs::create_dir_all(tree.join("infra/estate")).unwrap();
    write_file(&tree.join(RENDERER_REL), renderer_body);
}

/// THE OBJECT THE CONVERGE RENDERS PER INSTANCE IS NOT DELETABLE (backlog
/// cb0c9937). `ConfigMap/<instance ns>/boss-instance-config` is generated
/// by infra/estate/render-dev-door-config.sh and mounted by the boss pod;
/// no manifest declares it. From train #916 until this test the derivation
/// had no entry for it, so `--check` named it UNDECLARED — and this verb,
/// whose authority is that answer, would have deleted a live mount of the
/// operating instance on request (ConfigMap is inside its kind floor). The
/// derivation now derives the exemption from the renderer and the instance
/// list; this holds the verb to it, with the two controls that keep the
/// exemption from being wider than the renderer: another name in the same
/// namespace still deletes, and a renderer whose object cannot be read is
/// CANNOT ANSWER, never "no exemption".
#[test]
fn refuses_the_object_the_converge_renders_per_instance() {
    let (renderer, name) = shipped_renderer();
    let target = format!("ConfigMap/boss/{name}");

    let build = |case: &str, renderer_body: &str| {
        let mut c = Case::new(
            case,
            &[
                ("ConfigMap", "boss", &name, ""),
                ("ConfigMap", "boss", "hand-made", ""),
            ],
        );
        write_instance_and_renderer(&c.work, renderer_body);
        c.commit("the renderer");
        c
    };

    let c = build("rendered-instance-config", &renderer);
    for args in [vec![target.as_str()], vec![target.as_str(), "--dry-run"]] {
        let (rc, out) = c.run(&args);
        assert_eq!(
            rc, 2,
            "the verb did not REFUSE the object the converge renders ({args:?}):\n{out}"
        );
        contains_all(&out, &["REFUSED", &target, "EXEMPT"], "the rendered object");
    }
    assert_eq!(
        c.deletions(),
        "",
        "the rendered ConfigMap was deleted:\n{}",
        c.deletions()
    );
    // The control: the exemption is one name, not the kind.
    let (rc, out) = c.run(&["ConfigMap/boss/hand-made"]);
    assert_eq!(
        rc, 0,
        "a hand-made ConfigMap of another name is still an orphan:\n{out}"
    );
    assert_eq!(c.deletions().trim(), "ConfigMap\tboss\thand-made", "{out}");

    // A renderer whose line cannot be read: nothing is decided, nothing
    // deleted — not even the genuine orphan beside it.
    let c = build(
        "rendered-instance-config-unreadable",
        &renderer.replace("\nRENDERS=", "\nOBJECT="),
    );
    for t in [target.as_str(), "ConfigMap/boss/hand-made"] {
        let (rc, out) = c.run(&[t]);
        assert_eq!(
            rc, 1,
            "the verb answered {t} from an exemption set it could not derive:\n{out}"
        );
        contains_all(
            &out,
            &["CANNOT ANSWER", RENDERER_REL],
            "the unreadable case",
        );
    }
    assert_eq!(c.deletions(), "", "something was deleted");
}

/// An object that is not there. "Not found" is not "orphaned", and the
/// refusal says so rather than exiting 0 on a no-op.
#[test]
fn refuses_an_object_that_does_not_exist() {
    let c = Case::new("absent", &[]);
    let (rc, out) = c.run(&["Service/boss/boss-ghost"]);
    assert_ne!(
        rc, 0,
        "the verb accepted an object that is not live:\n{out}"
    );
    assert_eq!(c.deletions(), "", "something was deleted:\n{out}");
    contains_all(&out, &["boss-ghost", "not live"], "the absent case");
}

/// A namespace the tree does not own. The tree declares no `Namespace`
/// for it, so "undeclared" is not a question this derivation can answer
/// there — the cluster's own furniture is not BOSS's orphan.
#[test]
fn refuses_a_namespace_the_tree_does_not_own() {
    let c = Case::new("foreign-ns", &[("Service", "kube-system", "kube-dns", "")]);
    let (rc, out) = c.run(&["Service/kube-system/kube-dns"]);
    assert_ne!(
        rc, 0,
        "the verb accepted an object outside the owned namespaces:\n{out}"
    );
    assert_eq!(c.deletions(), "", "something was deleted:\n{out}");
    contains_all(&out, &["kube-system", "own"], "the foreign-namespace case");
}

/// A kind the tree declares nothing of in that namespace: there is no
/// declared set to compare against, so every object of it would look
/// undeclared.
#[test]
fn refuses_a_kind_the_tree_declares_nothing_of() {
    let c = Case::new("unscoped-kind", &[("Ingress", "boss", "boss-front", "")]);
    let (rc, out) = c.run(&["Ingress/boss/boss-front"]);
    assert_ne!(
        rc, 0,
        "the verb accepted a kind the tree declares nothing of:\n{out}"
    );
    assert_eq!(c.deletions(), "", "something was deleted:\n{out}");
    contains_all(&out, &["Ingress", "boss"], "the unscoped-kind case");
}

/// Secret is excluded from the derivation by design — the tree
/// references secrets by name and creates them out of band, so its
/// declaration set for Secrets is incomplete and a sweep against it
/// would call every legitimate one an orphan.
#[test]
fn refuses_an_excluded_kind() {
    let c = Case::new("excluded-kind", &[("Secret", "boss-dev", "some-token", "")]);
    let (rc, out) = c.run(&["Secret/boss-dev/some-token"]);
    assert_ne!(rc, 0, "the verb accepted a Secret:\n{out}");
    assert_eq!(c.deletions(), "", "a Secret was deleted:\n{out}");
    contains_all(&out, &["Secret"], "the excluded-kind case");
}

/// A controller made it from a declared parent. Deleting the parent's
/// manifest is the declared change; the child follows.
#[test]
fn refuses_a_controller_owned_object() {
    let c = Case::new(
        "owned",
        &[("Deployment", "boss", "boss-sidecar", "StatefulSet")],
    );
    let (rc, out) = c.run(&["Deployment/boss/boss-sidecar"]);
    assert_ne!(rc, 0, "the verb accepted a controller-owned object:\n{out}");
    assert_eq!(
        c.deletions(),
        "",
        "a controller-owned object was deleted:\n{out}"
    );
    contains_all(
        &out,
        &["boss-sidecar", "owner"],
        "the controller-owned case",
    );
}

/// "Could not look" must never read as "nothing to report". A credential
/// that cannot list the pair means the derivation cannot answer, and the
/// verb refuses rather than treating an unlistable object as undeclared.
#[test]
fn refuses_when_the_credential_cannot_list_the_pair() {
    let c = Case::new(
        "forbidden",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    std::fs::write(c.root.join("forbid"), "Service boss\n").unwrap();
    let (rc, out) = c.run(&["Service/boss/boss-docs-internal"]);
    assert_ne!(rc, 0, "the verb acted on an unreadable pair:\n{out}");
    assert_eq!(c.deletions(), "", "something was deleted:\n{out}");
    contains_all(&out, &["Service", "boss"], "the unreadable case");
}

/// A manifest that does not parse drops its objects from the declared
/// set, which would make everything it declares look undeclared. The
/// derivation must fail loudly instead of quietly scraping less.
#[test]
fn refuses_when_a_manifest_cannot_be_parsed() {
    let c = Case::new("badparse", &[("Service", "boss", "boss-docs-internal", "")]);
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join("infra/forge/delete-orphan-object.sh"))
        .arg("Service/boss/boss-docs-internal")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_KUBECTL", &c.kubectl)
        .env("BOSS_CLUSTER_TREE", &c.tree)
        .env("STUB_LIVE", &c.live)
        .env("STUB_DELETED", &c.deleted)
        .env("STUB_BADPARSE", "boss-jobs-internal.yaml");
    let out = cmd.output().expect("script runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(
        out.status.code(),
        Some(0),
        "the verb acted on a partial scrape:\n{text}"
    );
    assert_eq!(c.deletions(), "", "something was deleted:\n{text}");
    contains_all(
        &text,
        &["boss-jobs-internal.yaml", "parse"],
        "the bad-parse case",
    );
}

/// The verb's own floor, narrower than the derivation's scope: a kind
/// whose deletion destroys bytes or credentials stays a human step even
/// when the tree proves the object undeclared.
#[test]
fn refuses_a_kind_outside_the_verbs_floor() {
    let c = Case::new(
        "kind-floor",
        &[("PersistentVolumeClaim", "boss", "stray-data", "")],
    );
    let (rc, out) = c.run(&["PersistentVolumeClaim/boss/stray-data"]);
    assert_ne!(rc, 0, "the verb deleted a PVC:\n{out}");
    assert_eq!(c.deletions(), "", "a PVC was deleted:\n{out}");
    contains_all(&out, &["PersistentVolumeClaim"], "the kind-floor case");
    // And the narrowing is real: the derivation DOES name it, so the
    // refusal is the verb's floor and not a derivation miss.
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join("infra/cluster/undeclared-objects.sh"))
        .args(["--check", "PersistentVolumeClaim/boss/stray-data"])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_KUBECTL", &c.kubectl)
        .env("BOSS_CLUSTER_TREE", &c.tree)
        .env("STUB_LIVE", &c.live)
        .env("STUB_DELETED", &c.deleted);
    let d = cmd.output().expect("undeclared-objects.sh runs");
    assert_eq!(
        d.status.code(),
        Some(0),
        "the derivation did not name the PVC, so the kind floor is untested:\n{}{}",
        String::from_utf8_lossy(&d.stdout),
        String::from_utf8_lossy(&d.stderr)
    );
}

/// A malformed argument is refused before anything runs — and by the
/// script itself, not only by the allowlist, so the bound does not
/// depend on one layer.
#[test]
fn refuses_a_malformed_argument() {
    let c = Case::new("malformed", &[]);
    for bad in [
        "boss-docs-internal",
        "service/boss/x",
        "Service/boss",
        "Service//x",
        "",
    ] {
        let (rc, out) = c.run(&[bad]);
        assert_ne!(rc, 0, "the verb accepted {bad:?}:\n{out}");
        assert_eq!(c.deletions(), "", "{bad:?} deleted something:\n{out}");
        // And the refusal says what the shape is — so this case cannot
        // pass merely because the script is absent.
        contains_all(
            &out,
            &["<Kind>/<namespace>/<name>"],
            &format!("the {bad:?} case"),
        );
    }
}

// ---------------------------------------------------------------------------
// THE DECLARATION SET IS A VERIFIED GENERATION OF ROOT'S TREE.
// ---------------------------------------------------------------------------
// THE DEFECT (backlog 1dde630c, review a8b752d4 N6). The verb runs as root.
// Its declaration set used to be a CHECKOUT — on the forge the one the
// runner hands its verbs, BOSS_OPS_CHECKOUT=/home/david/boss, which its
// owner can edit — and its only check that the set was reviewed content
// was `git status` over infra/cluster/manifests. The derivation reads more
// than the manifests: instances.toml and the renderer's RENDERS line (the
// per-instance exemption), infra/gate-runner/*.yaml, the consist worker's
// job.json, the dev and isolation declarations. Any of those could be
// edited uncommitted, or replaced by a symlink root then followed, and
// change what root would delete.
//
// Root's own tree (infra/forge/root-tree.sh) is the answer that already
// exists: a root-owned export of a commit main named, beside root's own
// store. The verb now reads ONLY such a generation, and proves it is still
// that commit, byte for byte, against the store before deriving anything.
// The privilege drop to the checkout's owner went with the checkout: root's
// git in root's store needs none.

/// `delete-orphan-object.sh` as the ops runner starts it, unrun, so a case
/// can change the environment it is started with.
impl Case {
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join("infra/forge/delete-orphan-object.sh"))
            .args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("BOSS_KUBECTL", &self.kubectl)
            .env("BOSS_CLUSTER_TREE", &self.tree)
            .env("STUB_LIVE", &self.live)
            .env("STUB_DELETED", &self.deleted);
        cmd
    }
}

fn exit_and_text(mut cmd: Command) -> (i32, String) {
    let out = cmd.output().expect("delete-orphan-object.sh runs");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// THE PACKET'S OWN CASE. On the forge the runner starts this verb with
/// BOSS_CLUSTER_TREE unset and BOSS_OPS_CHECKOUT naming the owner's
/// checkout. There, an UNCOMMITTED edit to instances.toml — moving the
/// one instance out of `boss` — drops the exemption for the ConfigMap the
/// converge renders into `boss` and mounts, and the manifests directory is
/// still clean. Until this car the verb took that checkout, found nothing
/// dirty where it looked, and deleted the live mount. It takes no checkout
/// now: the tree it reads is the one it was started from, and that must
/// be a generation of root's tree.
#[test]
fn the_checkout_the_runner_hands_its_verbs_is_not_a_declaration_set() {
    let (renderer, name) = shipped_renderer();
    let mut c = Case::new("handed-checkout", &[("ConfigMap", "boss", &name, "")]);
    write_instance_and_renderer(&c.work, &renderer);
    c.commit("the renderer");
    write_file(
        &c.work.join("infra/cluster/instances.toml"),
        "source = \"prod\"\n\n[prod]\nnamespace = \"boss-dev\"\ntenant_dir = \"examples/brewery\"\nsim = false\nhostname = \"h.example\"\nguest = false\n",
    );
    let target = format!("ConfigMap/boss/{name}");
    let mut cmd = c.command(&[&target]);
    cmd.env_remove("BOSS_CLUSTER_TREE")
        .env("BOSS_OPS_CHECKOUT", &c.work);
    let (rc, out) = exit_and_text(cmd);
    assert_eq!(
        c.deletions(),
        "",
        "the verb deleted the rendered ConfigMap on the strength of an uncommitted edit in the checkout it was handed:\n{out}"
    );
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &["REFUSED", "not a generation of root's tree"],
        "the handed checkout",
    );
}

/// A checkout is not root's tree even when it is clean and committed: its
/// owner can edit it between this check and the derivation's read.
#[test]
fn refuses_a_tree_that_is_not_a_generation_of_roots_tree() {
    let c = Case::new(
        "not-a-generation",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    let mut cmd = c.command(&["Service/boss/boss-docs-internal", "--dry-run"]);
    cmd.env("BOSS_CLUSTER_TREE", &c.work);
    let (rc, out) = exit_and_text(cmd);
    assert_eq!(
        rc, 2,
        "the verb read a checkout as its declaration set:\n{out}"
    );
    contains_all(
        &out,
        &["REFUSED", "not a generation of root's tree"],
        "the checkout",
    );
    assert_eq!(c.deletions(), "", "{out}");
}

/// A generation says which commit it is, and that commit is in the store
/// beside it — or nothing about the tree can be proved.
#[test]
fn refuses_a_generation_that_cannot_be_proved_against_the_store() {
    // `.boss-tree-sha` naming another commit.
    let c = Case::new(
        "stamp-differs",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    write_file(
        &c.tree.join(".boss-tree-sha"),
        &format!("{}\n", "0".repeat(40)),
    );
    let (rc, out) = c.run(&["Service/boss/boss-docs-internal", "--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &["REFUSED", ".boss-tree-sha"],
        "a stamp naming another commit",
    );

    // A generation named, and stamped, for a commit the store does not hold.
    let mut c = Case::new(
        "not-in-store",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    let fake = "1".repeat(40);
    let moved = c.tree.parent().unwrap().join(&fake);
    std::fs::rename(&c.tree, &moved).unwrap();
    c.tree = moved;
    write_file(&c.tree.join(".boss-tree-sha"), &format!("{fake}\n"));
    let (rc, out) = c.run(&["Service/boss/boss-docs-internal", "--dry-run"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(
        &out,
        &["REFUSED", "does not hold", &fake[..12]],
        "a commit the store lacks",
    );
    assert_eq!(c.deletions(), "", "{out}");
}

/// THE GENERATION IS ITS COMMIT, OR IT IS NOT READ. Each tamper below is a
/// change root's tree never makes — it exports whole and never writes a
/// generation again — and each is refused naming the path, before the
/// derivation reads a byte. The packet's own file, instances.toml, is the
/// first: edited in the generation, it drops the rendered ConfigMap's
/// exemption exactly as it did in the checkout. Every other tamper asks
/// for `hand-made`, a genuine orphan the untampered tree deletes, so a
/// tamper that went unnoticed shows as a deletion, not as some other
/// refusal.
#[test]
fn refuses_a_generation_that_differs_from_its_commit() {
    let (renderer, name) = shipped_renderer();
    let rendered = format!("ConfigMap/boss/{name}");
    let orphan = "ConfigMap/boss/hand-made".to_string();
    type Tamper = fn(&Path, &Path);
    let tampers: [(&str, &str, &String, Tamper); 5] = [
        (
            "edited",
            "infra/cluster/instances.toml",
            &rendered,
            |tree, _| {
                write_file(
                    &tree.join("infra/cluster/instances.toml"),
                    "source = \"prod\"\n\n[prod]\nnamespace = \"boss-dev\"\ntenant_dir = \"examples/brewery\"\nsim = false\nhostname = \"h.example\"\nguest = false\n",
                )
            },
        ),
        (
            "deleted",
            "infra/cluster/manifests/boss-front.yaml",
            &orphan,
            |tree, _| {
                std::fs::remove_file(tree.join("infra/cluster/manifests/boss-front.yaml")).unwrap()
            },
        ),
        (
            "added",
            "infra/gate-runner/extra.yaml",
            &orphan,
            |tree, _| {
                // The derivation reads every yaml here as declared; git
                // tracks no empty directory, so the export has none.
                std::fs::create_dir_all(tree.join("infra/gate-runner")).unwrap();
                write_file(&tree.join("infra/gate-runner/extra.yaml"), "{}\n")
            },
        ),
        (
            "symlinked",
            "infra/cluster/manifests/boss-front.yaml",
            &orphan,
            |tree, root| {
                let manifest = tree.join("infra/cluster/manifests/boss-front.yaml");
                let outside = root.join("outside.yaml");
                std::fs::copy(&manifest, &outside).unwrap();
                std::fs::remove_file(&manifest).unwrap();
                std::os::unix::fs::symlink(&outside, &manifest).unwrap();
            },
        ),
        (
            "group-writable",
            "writable by its group",
            &orphan,
            |tree, _| chmod(tree, 0o775),
        ),
    ];
    for (how, names, target, tamper) in tampers {
        let mut c = Case::new(
            &format!("tampered-{how}"),
            &[
                ("ConfigMap", "boss", &name, ""),
                ("ConfigMap", "boss", "hand-made", ""),
            ],
        );
        write_instance_and_renderer(&c.work, &renderer);
        c.commit("the renderer");
        tamper(&c.tree, &c.root);
        let (rc, out) = c.run(&[target]);
        assert_eq!(
            c.deletions(),
            "",
            "{how}: the verb deleted on the strength of a generation that is not its commit:\n{out}"
        );
        assert_eq!(rc, 2, "{how}: {out}");
        contains_all(&out, &["REFUSED", names], how);
    }
}

// ---------------------------------------------------------------------------
// The derivation is ONE definition, with a list form the lint can use.
// ---------------------------------------------------------------------------

/// `--list` is the whole orphan set, which is what the orphan lint
/// reports and what `--check` answers one object of. Both come from the
/// same run of the same computation, so the verb cannot admit an object
/// the lint would call declared.
#[test]
fn the_list_and_the_check_agree() {
    let c = Case::new("list", &[("Service", "boss", "boss-docs-internal", "")]);
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join("infra/cluster/undeclared-objects.sh"))
        .arg("--list")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_KUBECTL", &c.kubectl)
        .env("BOSS_CLUSTER_TREE", &c.tree)
        .env("STUB_LIVE", &c.live);
    let out = cmd.output().expect("undeclared-objects.sh runs");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(
        out.status.code(),
        Some(0),
        "--list failed:\n{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>(),
        vec!["Service\tboss\tboss-docs-internal"],
        "--list is not exactly the one orphan:\n{text}"
    );
}

// ---------------------------------------------------------------------------
// THROUGH THE RUNNER, with the real allowlist.
// ---------------------------------------------------------------------------

/// The allowlist's own validation: a malformed param never reaches the
/// script, and the refusal names the pattern on the packet's step — the
/// same text the journal carries.
#[test]
fn the_allowlist_refuses_a_malformed_param() {
    if !has("jq") {
        eprintln!("skipping: the ops-runner is sh + jq and this box has no jq");
        return;
    }
    let c = Case::new(
        "runner-malformed",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    let verbs = rewritten_verbs(&c.root);
    let (text, meta) = run_runner(&c, &verbs, r#"["services/boss/x"]"#);
    let meta = meta.expect("the runner completed the execute step");
    assert_eq!(
        meta["disposition"], "refused",
        "a malformed param was not refused:\n{text}"
    );
    let reason = meta["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("object") && reason.contains("does not match"),
        "the refusal does not name the param and its pattern: {reason}"
    );
    assert_eq!(
        reason,
        meta["output"].as_str().unwrap_or_default(),
        "reason and output differ on a refusal"
    );
    assert!(
        text.contains(reason),
        "the journal line does not carry the same reason:\n{text}"
    );
    assert_eq!(
        c.deletions(),
        "",
        "a refused packet deleted something:\n{text}"
    );
}

/// And a well-formed one does reach the script, with `--dry-run` — the
/// verb is exercisable end to end through the audited door without
/// deleting anything.
#[test]
fn the_runner_answers_a_dry_run() {
    if !has("jq") {
        eprintln!("skipping: the ops-runner is sh + jq and this box has no jq");
        return;
    }
    let c = Case::new(
        "runner-dry-run",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    let verbs = rewritten_verbs(&c.root);
    let (text, meta) = run_runner(
        &c,
        &verbs,
        r#"["Service/boss/boss-docs-internal","--dry-run"]"#,
    );
    let meta = meta.expect("the runner completed the execute step");
    assert_eq!(
        meta["disposition"], "answered",
        "the dry run was not answered:\n{text}"
    );
    assert_eq!(
        meta["exit_code"], "0",
        "the dry run did not exit 0:\n{text}"
    );
    let output = meta["output"].as_str().unwrap_or_default();
    assert!(
        output.contains("DRY RUN"),
        "the output is not the dry run's:\n{output}"
    );
    assert_eq!(c.deletions(), "", "a dry run deleted something:\n{text}");
    assert!(
        meta["effect"]
            .as_str()
            .is_some_and(|e| e.contains("DRY RUN — would delete Service")),
        "the runner did not judge the dry run's line as its effect: {meta}"
    );
}

/// Through the door, a real delete's effect is the read-back line —
/// judged by the runner against the verb file's declared `effect`
/// (backlog 1058e686, car B: the verb left `effect_unread`).
#[test]
fn the_runner_records_a_real_deletes_read_back_as_its_effect() {
    if !has("jq") {
        eprintln!("skipping: the ops-runner is sh + jq and this box has no jq");
        return;
    }
    let c = Case::new(
        "runner-for-real",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    let verbs = rewritten_verbs(&c.root);
    let (text, meta) = run_runner(&c, &verbs, r#"["Service/boss/boss-docs-internal"]"#);
    let meta = meta.expect("the runner completed the execute step");
    assert_eq!(meta["exit_code"], "0", "{text}");
    assert!(
        meta["effect"].as_str().is_some_and(|e| e.contains(
            "delete-orphan-object: OK — Service `boss-docs-internal` is gone from `boss` (read back: NotFound)"
        )),
        "the effect is not the read-back line: {meta}"
    );
    assert!(meta["effect_unread"].is_null(), "{meta}");
}

/// The real `infra/ops/verbs/` — one file per verb — copied verbatim:
/// the allowlist under test is the one that ships. (Its script paths
/// are repo-relative since 66077f9c and the runner resolves them
/// against its own checkout, so nothing is rewritten.)
fn rewritten_verbs(root: &Path) -> PathBuf {
    let dst = root.join("verbs");
    std::fs::create_dir_all(&dst).unwrap();
    for e in std::fs::read_dir(repo_root().join("infra/ops/verbs")).expect("infra/ops/verbs/") {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "json") {
            std::fs::copy(&p, dst.join(p.file_name().unwrap())).unwrap();
        }
    }
    dst
}

/// One open ops-request for the forge carrying the verb and args, run
/// through `ops-runner.sh` against a stubbed system of record.
fn run_runner(c: &Case, verbs: &Path, args: &str) -> (String, Option<serde_json::Value>) {
    let bin = c.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exec(
        &bin.join("curl"),
        &[
            "#!/bin/sh\n",
            boss_testing::ops_runner_stub::RECORD_STEP_METADATA,
            "cat \"$STUB_JOBS\"\n",
        ]
        .concat(),
    );
    std::fs::write(
        c.root.join("jobs.json"),
        format!(
            r#"{{"data":[{{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{{"host":"forge","verb":"delete-orphan-object","args":{args}}},"steps":[{{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}]}}"#
        ),
    )
    .unwrap();
    let step_md = c.root.join("step-metadata.json");
    let _ = std::fs::remove_file(&step_md);
    let out = Command::new("sh")
        .arg(repo_root().join("infra/ops/ops-runner.sh"))
        .env_clear()
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOST_ID", "forge")
        .env("BOSS_JOBS_URL", "http://sor.invalid")
        .env("OPS_VERBS_DIR", verbs)
        .env("STUB_JOBS", c.root.join("jobs.json"))
        .env("STUB_STEP_METADATA", &step_md)
        .env("BOSS_KUBECTL", &c.kubectl)
        .env("BOSS_CLUSTER_TREE", &c.tree)
        .env("STUB_LIVE", &c.live)
        .env("STUB_DELETED", &c.deleted)
        .output()
        .expect("ops-runner.sh runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let meta = boss_testing::ops_runner_stub::step_metadata_written(&step_md);
    (text, meta)
}
