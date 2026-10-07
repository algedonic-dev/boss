//! `infra/cluster/undeclared-objects.sh` is RUN, not read — against a
//! stubbed `kubectl` and a fixture manifest tree, so every verdict below
//! is one the script actually reached. Nothing here touches a cluster.
//!
//! WHY THIS FILE EXISTS, SEPARATELY FROM THE VERB'S (backlog 19aa75e0).
//! The derivation answers one question — "what is running that the tree
//! does not declare" — and two programs read the answer: the orphan lint
//! reports it, and `delete-orphan-object` DERIVES ITS AUTHORITY from it.
//! So the failure mode that matters is not a wrong orphan; it is a
//! SMALLER declared set. A manifest that will not parse drops every
//! object it declares out of the declared set, and a declared object
//! then reads as UNDECLARED — which, at the other end of the verb, is a
//! path to deleting it.
//!
//! `delete_orphan_object_sh.rs` covers that through `--check`, the one
//! mode the verb calls. It is the narrow mode: `--list` and `--declared`
//! are the modes the LINT reads, and their refusal was pinned by
//! nothing. These tests pin all three, and pin the two properties a
//! refusal needs in order to be read as one:
//!
//!   * it NAMES the file that would not parse, and
//!   * its exit code is distinguishable from both "clean" and "found an
//!     orphan" — which in `--list` share exit 0, so a consumer that only
//!     tests `-ne 0` cannot tell a refusal from a finding.
//!
//! Plus the reasons a refusal must carry rather than drop: a pair this
//! credential cannot list, and an object it cannot read, each with the
//! server's own words. "Could not look" reduced to a count is the
//! record thrown away before the reader sees it (CLAUDE.md §Diagnosis).
//!
//! THE LINT IS EXERCISED HERE TOO, out of the same fixture, because it is
//! the consumer — and because the other half of 19aa75e0 was that it held
//! a SECOND copy of these rules: its own $EXCLUDED_KINDS, $EXEMPT,
//! $EXEMPT_ANY_NS, $is_exempt and its own sweep of the cluster. Two
//! definitions of "what the tree does not declare" differ in exactly the
//! places nobody compared (CLAUDE.md §9a), and at the far end of
//! `delete-orphan-object` a divergence is a path to deleting a DECLARED
//! object. So the lint's half of this file asserts a COLLAPSE rather than
//! a second copy: make the two disagree, and the lint must follow the
//! derivation instead of itself. A test that only checked the answers
//! would pass just as well against two copies that happen to agree today.

use boss_testing::{repo_root, write_exec};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The derivation's exit code for "cannot answer" — distinct from 0
/// (answered; stdout is the answer), 1 (a usage error: this run asked
/// nothing well-formed) and 3 (`--check` says NO). The verb maps every
/// code other than 0 and 3 to CANNOT ANSWER already, so this is a
/// sharpening of the contract, not a break in it.
const CANNOT_ANSWER: i32 = 4;

/// A scratch directory THIS uid and THIS process own outright. `/tmp` is
/// sticky, so a fixed name left behind by another uid is a directory this
/// run can neither remove nor write — the gate runs as 65534 and a
/// developer as root, and a test that fails on whichever ran second is a
/// test that reds cars for no reason.
fn scratch(case: &str) -> PathBuf {
    boss_testing::scratch_dir(&format!("undeclared-objects-{case}"))
}

/// One object, as JSON. The fixture writes JSON into `.yaml` files —
/// JSON is valid YAML, and `kubectl create --dry-run -o json -f` echoes
/// it back, so the stub can simply `cat`.
fn doc(kind: &str, ns: &str, name: &str) -> String {
    if ns.is_empty() {
        format!(r#"{{"kind":"{kind}","metadata":{{"name":"{name}"}}}}"#)
    } else {
        format!(r#"{{"kind":"{kind}","metadata":{{"name":"{name}","namespace":"{ns}"}}}}"#)
    }
}

/// Every object the fixture tree declares in a namespace it owns, as
/// `(kind, ns, name)`. The live set is built from this, so the fixture's
/// cluster agrees with its tree except where a case says otherwise.
fn declared_in_namespaces() -> Vec<(&'static str, &'static str, &'static str)> {
    let mut v = vec![
        ("ConfigMap", "boss", "boss-config"),
        ("Deployment", "boss", "boss"),
        ("CronJob", "boss", "boss-backup"),
        ("ServiceAccount", "boss", "boss"),
        ("PersistentVolumeClaim", "boss", "boss-auth"),
        ("Role", "boss-dev", "dev-session"),
        ("RoleBinding", "boss-dev", "dev-session"),
        ("Service", "boss-dev", "boss-dev"),
        ("Deployment", "boss-dev", "boss-dev"),
    ];
    for n in SERVICES {
        v.push(("Service", "boss", n));
    }
    v
}

/// Twelve Services in `boss`, one manifest each — enough files and
/// objects that the derivation's "the scrape broke" floors (at least 10
/// files, at least 20 objects) are cleared by the fixture rather than
/// relaxed for it.
const SERVICES: [&str; 12] = [
    "svc-a", "svc-b", "svc-c", "svc-d", "svc-e", "svc-f", "svc-g", "svc-h", "svc-i", "svc-j",
    "svc-k", "svc-l",
];

/// The manifest tree, laid out like the repo.
fn fixture_tree(root: &Path) -> PathBuf {
    let tree = root.join("tree");
    let dir = tree.join("infra/cluster/manifests");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(tree.join("infra/gate-runner")).unwrap();

    let write = |name: &str, body: String| std::fs::write(dir.join(name), body).unwrap();
    write(
        "namespaces.yaml",
        format!(
            "{}\n{}\n",
            doc("Namespace", "", "boss"),
            doc("Namespace", "", "boss-dev")
        ),
    );
    write(
        "cluster-scoped.yaml",
        format!(
            "{}\n{}\n",
            doc("ClusterRole", "", "boss-reader"),
            doc("StorageClass", "", "longhorn-r1")
        ),
    );
    write(
        "configmaps.yaml",
        format!("{}\n", doc("ConfigMap", "boss", "boss-config")),
    );
    write(
        "deployment.yaml",
        format!("{}\n", doc("Deployment", "boss", "boss")),
    );
    write(
        "cronjob.yaml",
        format!("{}\n", doc("CronJob", "boss", "boss-backup")),
    );
    write(
        "serviceaccount.yaml",
        format!("{}\n", doc("ServiceAccount", "boss", "boss")),
    );
    write(
        "pvc.yaml",
        format!("{}\n", doc("PersistentVolumeClaim", "boss", "boss-auth")),
    );
    write(
        "dev-access.yaml",
        format!(
            "{}\n{}\n",
            doc("Role", "boss-dev", "dev-session"),
            doc("RoleBinding", "boss-dev", "dev-session")
        ),
    );
    write(
        "dev.yaml",
        format!(
            "{}\n{}\n",
            doc("Service", "boss-dev", "boss-dev"),
            doc("Deployment", "boss-dev", "boss-dev")
        ),
    );
    for n in SERVICES {
        write(
            &format!("{n}.yaml"),
            format!("{}\n", doc("Service", "boss", n)),
        );
    }
    tree
}

/// The stubbed `kubectl`, answering the shapes the derivation uses and
/// reading the "cluster" from files:
///
///   $STUB_LIVE/<Kind>.<ns>  one line per live object: name<TAB>ownerKind[<TAB>part-of]
///   $STUB_FORBID            "<Kind> <ns>" per line — not listable
///   $STUB_FORBID_GET        "<Kind> <ns> <name>" per line — not readable
///   $STUB_BADPARSE          a manifest basename that will not parse
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
listed_in() { # file key
    [ -n "${1:-}" ] && [ -f "$1" ] && grep -qxF "$2" "$1"
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
    ns=""; sel=""; out=""
    while [ $# -gt 0 ]; do
        case "$1" in
            -n) ns="$2"; shift 2 ;;
            -o) out="$2"; shift 2 ;;
            -l) sel="$2"; shift 2 ;;
            *) shift ;;
        esac
    done
    if listed_in "${STUB_FORBID:-}" "$kind $ns"; then
        echo "Error from server (Forbidden): cannot list $kind in $ns with this credential" >&2
        exit 1
    fi
    if [ -z "$name" ]; then
        if [ "$out" = json ]; then
            cat "$STUB_LIVE/$kind.$ns.json"
            exit $?
        fi
        # A label selector keeps the rows it matches. The third column is
        # either the bare part-of value (the tree's own mark) or, when it
        # holds an `=`, the object's full `key=value,...` label set.
        if [ -n "$sel" ]; then
            live "$kind" "$ns" | awk -F'\t' -v s="$sel" '
                BEGIN { k = s; sub(/=.*/, "", k); w = s; sub(/^[^=]*=/, "", w) }
                index($3, "=") == 0 { if (k == "app.kubernetes.io/part-of" && $3 == w) print; next }
                { n = split($3, a, ","); for (i = 1; i <= n; i++) if (a[i] == s) { print; next } }'
        else
            live "$kind" "$ns"
        fi
        exit 0
    fi
    if listed_in "${STUB_FORBID_GET:-}" "$kind $ns $name"; then
        echo "Error from server (Forbidden): cannot get $kind $name in $ns with this credential" >&2
        exit 1
    fi
    row=$(live "$kind" "$ns" | awk -F'\t' -v n="$name" '$1 == n')
    if [ -z "$row" ]; then
        echo "Error from server (NotFound): $kind \"$name\" not found" >&2
        exit 1
    fi
    # A single get answers the owner, or the part-of label when the
    # jsonpath asks for labels.
    case "$out" in
        *labels*) printf '%s\n' "$row" | awk -F'\t' '{ print $3 }' ;;
        *) printf '%s\n' "$row" | cut -f2 ;;
    esac ;;
*)
    echo "stub kubectl: unexpected invocation: $*" >&2
    exit 64 ;;
esac
"#,
    );
    path
}

struct Case {
    root: PathBuf,
    tree: PathBuf,
    kubectl: PathBuf,
    live: PathBuf,
}

impl Case {
    /// A case whose cluster is the fixture's declared set plus `extra`
    /// (`kind, ns, name, ownerKind`).
    fn new(name: &str, extra: &[(&str, &str, &str, &str)]) -> Self {
        let root = scratch(name);
        let tree = fixture_tree(&root);
        let kubectl = stub_kubectl(&root);
        let live = root.join("live");
        std::fs::create_dir_all(&live).unwrap();
        let mut rows: Vec<(&str, &str, &str, &str)> = declared_in_namespaces()
            .into_iter()
            .map(|(k, ns, n)| (k, ns, n, ""))
            .collect();
        rows.extend_from_slice(extra);
        for (kind, ns, name, owner) in rows {
            let f = live.join(format!("{kind}.{ns}"));
            let mut body = std::fs::read_to_string(&f).unwrap_or_default();
            body.push_str(&format!("{name}\t{owner}\n"));
            std::fs::write(&f, body).unwrap();
        }
        Case {
            root,
            tree,
            kubectl,
            live,
        }
    }

    /// Put one more object into the fixture's cluster after construction.
    fn add_live(&self, kind: &str, ns: &str, name: &str, owner: &str) {
        let f = self.live.join(format!("{kind}.{ns}"));
        let mut body = std::fs::read_to_string(&f).unwrap_or_default();
        body.push_str(&format!("{name}\t{owner}\n"));
        std::fs::write(&f, body).unwrap();
    }

    /// One more object carrying `app.kubernetes.io/part-of=<label>` —
    /// the tree's own mark on what it created.
    fn add_live_labelled(&self, kind: &str, ns: &str, name: &str, label: &str) {
        let f = self.live.join(format!("{kind}.{ns}"));
        let mut body = std::fs::read_to_string(&f).unwrap_or_default();
        body.push_str(&format!("{name}\t\t{label}\n"));
        std::fs::write(&f, body).unwrap();
    }

    /// `(exit code, stdout, stdout+stderr)`. stdout is kept apart
    /// because in `--list` it IS the answer: a refusal must leave it
    /// empty, not merely exit nonzero.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, String)]) -> (i32, String, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join("infra/cluster/undeclared-objects.sh"))
            .args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("BOSS_KUBECTL", &self.kubectl)
            .env("BOSS_CLUSTER_TREE", &self.tree)
            .env("STUB_LIVE", &self.live);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("undeclared-objects.sh runs");
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let all = format!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), stdout, all)
    }

    /// Declare `Kind <ns>` unlistable by this credential.
    fn forbid(&self, pairs: &[(&str, &str)]) -> (&'static str, String) {
        let p = self.root.join("forbid");
        let body: String = pairs.iter().map(|(k, ns)| format!("{k} {ns}\n")).collect();
        std::fs::write(&p, body).unwrap();
        ("STUB_FORBID", p.display().to_string())
    }

    /// Declare one object unreadable by this credential.
    fn forbid_get(&self, kind: &str, ns: &str, name: &str) -> (&'static str, String) {
        let p = self.root.join("forbid-get");
        std::fs::write(&p, format!("{kind} {ns} {name}\n")).unwrap();
        ("STUB_FORBID_GET", p.display().to_string())
    }

    fn badparse(&self, basename: &str) -> (&'static str, String) {
        ("STUB_BADPARSE", basename.to_string())
    }
}

fn names_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(
            text.contains(n),
            "{what}: the output never names {n:?}:\n{text}"
        );
    }
}

// ---------------------------------------------------------------------------
// The answer, when the derivation can reach one.
// ---------------------------------------------------------------------------

/// The baseline every refusal below is measured against: a tree and a
/// cluster that agree, and an empty orphan set reported as such. Here so
/// that "it refuses" can never be satisfied by refusing everything.
#[test]
fn list_answers_cleanly_when_the_tree_and_the_cluster_agree() {
    let c = Case::new("clean", &[]);
    let (rc, stdout, all) = c.run(&["--list"]);
    assert_eq!(rc, 0, "a clean tree did not answer:\n{all}");
    assert_eq!(stdout, "", "a clean tree named an orphan:\n{all}");
    names_all(&all, &["0 undeclared object(s)"], "the clean case");
}

/// A live object no manifest declares IS the finding, and it rides on
/// stdout so a consumer can read it.
#[test]
fn list_names_a_genuine_orphan_on_stdout() {
    let c = Case::new("orphan", &[("Service", "boss", "boss-docs-internal", "")]);
    let (rc, stdout, all) = c.run(&["--list"]);
    assert_eq!(rc, 0, "the orphan case did not answer:\n{all}");
    assert_eq!(
        stdout.trim(),
        "Service\tboss\tboss-docs-internal",
        "the orphan set is not what the cluster holds:\n{all}"
    );
}

/// The converge GENERATES ConfigMap/<ns>/boss-tenant for every
/// repo-sourced instance (instances.toml `tenant_repo`, f4f5c387). No
/// manifest declares it and it is absent whenever that instance was
/// skipped, so it is exempt WHEN PRESENT and never stale when absent —
/// derived from the instance list, not typed into $EXEMPT. Measured
/// 2026-09-16 23:55Z: the first converge after the prod flip rolled prod
/// and then failed its own orphan check on exactly this object (d7d23650).
#[test]
fn a_repo_sourced_instances_delivered_tenant_is_exempt_by_derivation_and_never_stale() {
    let c = Case::new(
        "derived-tenant",
        &[("ConfigMap", "boss", "boss-tenant", "")],
    );
    // Without an instance list the object is an orphan: nothing derives it.
    let (rc, stdout, all) = c.run(&["--list"]);
    assert_eq!(rc, 0, "{all}");
    assert_eq!(stdout.trim(), "ConfigMap\tboss\tboss-tenant", "{all}");
    let (rc, derived, _) = c.run(&["--exemptions-derived"]);
    assert_eq!(
        (rc, derived.trim()),
        (0, ""),
        "no instance list, nothing derived"
    );

    // With prod declared repo-sourced and the playground image-sourced,
    // exactly prod's delivered ConfigMap is derived — and the object
    // reads clean.
    std::fs::write(
        c.tree.join("infra/cluster/instances.toml"),
        "source = \"prod\"\n\n[prod]\nnamespace = \"boss\"\ntenant_repo = \"david/algedonic-llc\"\ntenant_ref = \"main\"\nsim = false\nhostname = \"h.example\"\nguest = false\n\n[playground]\nnamespace = \"boss-playground\"\ntenant_dir = \"examples/brewery\"\nsim = true\nhostname = \"p.example\"\nguest = \"audit\"\n",
    )
    .unwrap();
    let (rc, derived, all) = c.run(&["--exemptions-derived"]);
    assert_eq!(rc, 0, "{all}");
    assert_eq!(derived.trim(), "ConfigMap/boss/boss-tenant");
    let (rc, hand, _) = c.run(&["--exemptions"]);
    assert_eq!(rc, 0);
    assert!(
        !hand.contains("boss-tenant"),
        "a derived exemption is not a hand entry, so the lint's stale check never asks the \
         cluster for it: {hand}"
    );
    let (rc, stdout, all) = c.run(&["--list"]);
    assert_eq!(rc, 0, "{all}");
    assert_eq!(
        stdout.trim(),
        "",
        "the delivered tenant is not an orphan:\n{all}"
    );
    // Only the repo-sourced instance derives one: the image-sourced
    // playground is not in the derived list (a boss-tenant there would
    // be an orphan in a tree that manages its namespace).
    assert_eq!(
        derived.lines().count(),
        1,
        "one derived exemption, prod's: {derived}"
    );
}

/// The instance list both instances of the estate have: prod repo-sourced
/// in `boss`, the playground image-sourced in `boss-playground`.
const TWO_INSTANCES: &str = "source = \"prod\"\n\n[prod]\nnamespace = \"boss\"\ntenant_repo = \"david/algedonic-llc\"\ntenant_ref = \"main\"\nsim = false\nhostname = \"h.example\"\nguest = false\n\n[playground]\nnamespace = \"boss-playground\"\ntenant_dir = \"examples/brewery\"\nsim = true\nhostname = \"p.example\"\nguest = \"audit\"\n";

/// The renderer of the per-instance runtime ConfigMap, and the converge
/// that calls it.
const RENDERER_REL: &str = "infra/estate/render-dev-door-config.sh";
const RUNNER_REL: &str = "infra/forge/cluster-deploy-runner.sh";

/// The `Kind/name` the SHIPPED renderer says it renders — read off its
/// one `RENDERS=` line the way the derivation reads it, so no test here
/// types the object's name a second time.
fn rendered_object() -> (String, String) {
    let body = std::fs::read_to_string(repo_root().join(RENDERER_REL)).expect("the renderer");
    let lines: Vec<&str> = body.lines().filter(|l| l.starts_with("RENDERS=")).collect();
    assert_eq!(
        lines.len(),
        1,
        "{RENDERER_REL} must name the object it renders on exactly one `RENDERS=\"Kind/name\"` \
         line at column 0 — infra/cluster/undeclared-objects.sh derives its exemption from it \
         (backlog cb0c9937). Found: {lines:?}"
    );
    let value = lines[0]
        .trim_start_matches("RENDERS=")
        .trim_matches('"')
        .to_string();
    let (kind, name) = value
        .split_once('/')
        .unwrap_or_else(|| panic!("{RENDERER_REL}: `{}` is not Kind/name", lines[0]));
    (kind.to_string(), name.to_string())
}

/// Put the shipped renderer into a fixture tree, optionally rewritten.
fn install_renderer(tree: &Path, rewrite: impl Fn(&str) -> String) {
    let dst = tree.join(RENDERER_REL);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    let body = std::fs::read_to_string(repo_root().join(RENDERER_REL)).expect("the renderer");
    std::fs::write(&dst, rewrite(&body)).unwrap();
}

/// THE DEFECT (backlog cb0c9937, measured 2026-10-06). The converge
/// renders one runtime ConfigMap per instance namespace
/// (render-dev-door-config.sh, called for the source instance and for
/// every other applied one), no manifest declares it, and the derivation
/// did not know: converge packets 8d5c8f93 and 79f23714 both closed
/// FAILED at `check orphans` on `ConfigMap/boss-instance-config (ns
/// boss)`, and the stage after it — rolling the playground — never ran.
///
/// It is exempt BY DERIVATION: the name from the renderer's own line, the
/// namespaces from the instance list the converge walks. And only that —
/// the three controls are the point of the test:
///   * a hand-made ConfigMap of another name in `boss` is still an orphan;
///   * the SAME name in a namespace no instance has (`boss-dev`, the
///     pipeline's) is still an orphan; and
///   * `--check`, which is where `delete-orphan-object` gets its
///     authority, answers NO for the rendered object and YES for both
///     controls.
#[test]
fn the_rendered_instance_config_is_exempt_by_derivation_in_the_instances_namespaces_only() {
    let (kind, name) = rendered_object();
    assert_eq!(kind, "ConfigMap", "the fixture below assumes a ConfigMap");
    let c = Case::new(
        "derived-instance-config",
        &[
            ("ConfigMap", "boss", &name, ""),
            ("ConfigMap", "boss", "hand-made", ""),
        ],
    );
    // boss-dev declares no ConfigMap in the fixture, so the object is in
    // scope there by the tree's own label.
    c.add_live_labelled("ConfigMap", "boss-dev", &name, "boss");
    let rendered_boss = format!("ConfigMap/boss/{name}");
    let rendered_dev = format!("ConfigMap/boss-dev/{name}");

    // A tree that renders nothing (no renderer, no instance list) accounts
    // for none of them — so the exemption below is the derivation's doing.
    let (rc, stdout, all) = c.run(&["--list"]);
    assert_eq!(rc, 0, "{all}");
    let mut before: Vec<&str> = stdout.lines().collect();
    before.sort_unstable();
    let row_boss = format!("ConfigMap\tboss\t{name}");
    let row_dev = format!("ConfigMap\tboss-dev\t{name}");
    let mut all_three = vec!["ConfigMap\tboss\thand-made", &row_boss, &row_dev];
    all_three.sort_unstable();
    assert_eq!(before, all_three, "{all}");

    std::fs::write(c.tree.join("infra/cluster/instances.toml"), TWO_INSTANCES).unwrap();
    install_renderer(&c.tree, |b| b.to_string());

    let (rc, derived, all) = c.run(&["--exemptions-derived"]);
    assert_eq!(rc, 0, "{all}");
    let mut derived: Vec<&str> = derived.lines().collect();
    derived.sort_unstable();
    let rendered_playground = format!("ConfigMap/boss-playground/{name}");
    let mut want = vec![
        "ConfigMap/boss/boss-tenant",
        rendered_boss.as_str(),
        rendered_playground.as_str(),
    ];
    want.sort_unstable();
    assert_eq!(
        derived, want,
        "one rendered object per INSTANCE namespace — the converge calls the renderer for the \
         source instance and for every other one — and none for boss-dev, which is no instance"
    );
    let (rc, hand, _) = c.run(&["--exemptions"]);
    assert_eq!(rc, 0);
    assert!(
        !hand.contains(&name),
        "a derived exemption is not a hand entry — the lint's stale check would ask the cluster \
         for it on a converge that skipped the instance: {hand}"
    );

    let (rc, stdout, all) = c.run(&["--list"]);
    assert_eq!(rc, 0, "{all}");
    let mut after: Vec<&str> = stdout.lines().collect();
    after.sort_unstable();
    assert_eq!(
        after,
        vec!["ConfigMap\tboss\thand-made", &row_dev],
        "the rendered object is accounted for, and ONLY it: a hand-made ConfigMap of another \
         name, and the same name where no instance lives, are still findings:\n{all}"
    );

    // The verb's authority. Exit 3 is NO — `delete-orphan-object` refuses.
    let (rc, stdout, all) = c.run(&["--check", &rendered_boss]);
    assert_eq!(
        rc, 3,
        "the object the converge renders was named DELETABLE:\n{all}"
    );
    assert_eq!(stdout, "", "a refusal prints no `undeclared` row:\n{all}");
    names_all(&all, &["REFUSED", &rendered_boss, "EXEMPT"], "the check");
    for still in ["ConfigMap/boss/hand-made", rendered_dev.as_str()] {
        let (rc, stdout, all) = c.run(&["--check", still]);
        assert_eq!(rc, 0, "{still} is still undeclared:\n{all}");
        assert!(stdout.starts_with("undeclared\tConfigMap\t"), "{all}");
    }
}

/// A RENDERER THE DERIVATION CANNOT READ IS NOT A TREE THAT RENDERS
/// NOTHING. If the `RENDERS=` line is gone (renamed, split over two
/// lines, made a computed value) the exemption silently disappears, the
/// rendered object reads as undeclared, and `--check` would hand
/// `delete-orphan-object` the authority to delete a live mount of the
/// operating instance. So every mode that uses the exemptions says
/// CANNOT ANSWER and names the file.
#[test]
fn a_renderer_whose_object_cannot_be_read_is_cannot_answer_not_no_exemption() {
    let (_, name) = rendered_object();
    let c = Case::new(
        "derived-instance-config-unreadable",
        &[("ConfigMap", "boss", &name, "")],
    );
    std::fs::write(c.tree.join("infra/cluster/instances.toml"), TWO_INSTANCES).unwrap();
    install_renderer(&c.tree, |b| b.replace("\nRENDERS=", "\nOBJECT="));
    let target = format!("ConfigMap/boss/{name}");
    for args in [
        vec!["--list"],
        vec!["--check", target.as_str()],
        vec!["--exemptions-derived"],
    ] {
        let (rc, stdout, all) = c.run(&args);
        assert_eq!(
            rc, CANNOT_ANSWER,
            "{args:?} answered from an exemption set it could not derive:\n{all}"
        );
        assert_eq!(stdout, "", "{args:?} printed an answer:\n{all}");
        names_all(&all, &[RENDERER_REL, "RENDERS="], "the unreadable renderer");
    }
}

/// WHERE THE RENDERER IS CALLED is the other half of the derivation, and
/// it lives in the converge: `converge_dev_door` for the source
/// instance's namespace and for each other instance the apply loop
/// reaches. The derivation says "every instance namespace"; this holds
/// the converge to that, by name, so a third call site (a namespace that
/// is no instance) or a removed one fails here and not as an orphan on
/// every converge (CLAUDE.md §9a: a fact that lives twice gets a test).
#[test]
fn the_converge_renders_the_instance_config_for_instances_and_nothing_else() {
    let body = std::fs::read_to_string(repo_root().join(RUNNER_REL)).expect("the runner");
    let calls: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("converge_dev_door ") && !l.ends_with('{'))
        .collect();
    assert_eq!(
        calls,
        vec![
            "converge_dev_door \"$SOURCE_NS\"",
            "converge_dev_door \"$ins_ns\""
        ],
        "{RUNNER_REL} must render the instance config for the source instance and for each other \
         instance of infra/cluster/instances.toml, and nowhere else: {DERIVE_REL} \
         derived_exemptions accounts for exactly those namespaces. Change both together."
    );
    assert!(
        body.contains(&format!("\"$REPO/{RENDERER_REL}\"")),
        "{RUNNER_REL} no longer renders through {RENDERER_REL}, the file {DERIVE_REL} reads the \
         rendered object's name from"
    );
    let renderers: Vec<&str> = body
        .lines()
        .filter(|l| l.contains("render-dev-door-config.sh"))
        .collect();
    assert_eq!(
        renderers.len(),
        1,
        "the renderer has one caller in {RUNNER_REL}, inside converge_dev_door: {renderers:?}"
    );
}

/// WHY #916 WAS NOT CAUGHT AT THE GATE, closed. Train #916 added a mount
/// of a ConfigMap no manifest declares and a renderer for it, and no
/// check tied the two to the orphan derivation — so the first notice was
/// a FAILED converge, and then every one after it. A gate has no cluster,
/// but it has the tree: a ConfigMap a manifest REFERENCES (a volume, an
/// envFrom, a key ref) is one the pod expects to find, so it is either
/// DECLARED by a manifest or GENERATED by something — and a generated one
/// is exactly what the derivation must exempt, by hand entry or by
/// derivation, in the namespace the reference is in. Anything else is the
/// next `boss-instance-config`, named here with its file and line.
///
/// Read as text, deliberately narrow: the gate image has no kubectl and
/// no YAML parser, so this recognises the two spellings the tree uses —
/// `configMap: {name: X …}` inline and a `configMap:` block whose `name:`
/// follows — and FAILS on a reference it cannot name rather than skipping
/// it.
#[test]
fn every_configmap_a_manifest_mounts_is_declared_or_exempt_in_the_derivation() {
    let root = repo_root();
    let reference =
        regex::Regex::new(r"^(\s*)(?:-\s+)?configMap(?:Ref|KeyRef)?:\s*(.*)$").expect("regex");
    let inline_name = regex::Regex::new(r"\bname:\s*([A-Za-z0-9.-]+)").expect("regex");
    let top_kind = regex::Regex::new(r"^kind:\s*(\S+)").expect("regex");
    let meta_key = regex::Regex::new(r"^  (name|namespace):\s*(\S+)").expect("regex");

    let mut files: Vec<PathBuf> = Vec::new();
    for dir in ["infra/cluster/manifests", "infra/gate-runner"] {
        for entry in std::fs::read_dir(root.join(dir)).expect("manifest dir") {
            let p = entry.expect("dir entry").path();
            if p.extension().is_some_and(|e| e == "yaml") {
                files.push(p);
            }
        }
    }
    files.sort();
    assert!(
        files.len() >= 10,
        "the scrape broke: {} manifests",
        files.len()
    );

    // (namespace, name) of every ConfigMap a manifest declares, and
    // (file:line, namespace, name) of every reference.
    let mut declared: Vec<(String, String)> = Vec::new();
    let mut references: Vec<(String, String, String)> = Vec::new();
    for path in &files {
        let rel = path.strip_prefix(&root).unwrap().display().to_string();
        let body = std::fs::read_to_string(path).expect("manifest");
        let lines: Vec<&str> = body.lines().collect();
        let mut start = 0;
        let mut line_no = 0;
        while start <= lines.len() {
            let end = lines[start..]
                .iter()
                .position(|l| l.trim_end() == "---")
                .map_or(lines.len(), |i| start + i);
            let doc = &lines[start..end];
            let kind = doc
                .iter()
                .find_map(|l| top_kind.captures(l))
                .map(|c| c[1].to_string());
            // The object's own metadata: the first two-space `name:` and
            // `namespace:` after the top-level `metadata:` line.
            let (mut name, mut ns) = (None, None);
            if let Some(m) = doc.iter().position(|l| l.trim_end() == "metadata:") {
                for l in doc[m + 1..].iter().take_while(|l| !top_level(l)) {
                    if let Some(c) = meta_key.captures(l) {
                        let v = c[2].trim_matches('"').to_string();
                        match &c[1] {
                            "name" if name.is_none() => name = Some(v),
                            "namespace" if ns.is_none() => ns = Some(v),
                            _ => {}
                        }
                    }
                }
            }
            if kind.as_deref() == Some("ConfigMap")
                && let (Some(ns), Some(name)) = (ns.clone(), name.clone())
            {
                declared.push((ns, name));
            }
            for (i, l) in doc.iter().enumerate() {
                if l.trim_start().starts_with('#') {
                    continue;
                }
                let Some(c) = reference.captures(l) else {
                    continue;
                };
                let at = format!("{rel}:{}", line_no + i + 1);
                let indent = c[1].len();
                let rest = c[2].trim();
                let found = if rest.is_empty() || rest.starts_with('#') {
                    // Block form: the `name:` among the deeper-indented
                    // lines that follow.
                    doc[i + 1..]
                        .iter()
                        .take_while(|n| {
                            n.trim().is_empty() || n.len() - n.trim_start().len() > indent
                        })
                        .filter(|n| !n.trim_start().starts_with('#'))
                        .find_map(|n| {
                            n.trim_start()
                                .strip_prefix("name:")
                                .map(|v| v.trim().trim_matches('"').to_string())
                        })
                } else {
                    inline_name.captures(rest).map(|c| c[1].to_string())
                };
                let found = found.unwrap_or_else(|| {
                    panic!(
                        "{at}: a ConfigMap reference this reader cannot name: `{}`. Not being \
                         able to read it is not the same as there being none — teach this test \
                         the spelling.",
                        l.trim()
                    )
                });
                let ns = ns.clone().unwrap_or_else(|| {
                    panic!("{at}: references ConfigMap `{found}` from an object with no namespace")
                });
                references.push((at, ns, found));
            }
            line_no += doc.len() + 1;
            start = end + 1;
        }
    }
    assert!(
        references.len() >= 5 && !declared.is_empty(),
        "the scrape broke: {} reference(s), {} declared ConfigMap(s)",
        references.len(),
        declared.len()
    );

    let ask = |mode: &str| -> String {
        let out = Command::new("bash")
            .arg(root.join(DERIVE_REL))
            .arg(mode)
            .env_remove("BOSS_CLUSTER_TREE")
            .output()
            .expect("the derivation runs");
        assert!(
            out.status.success(),
            "{DERIVE_REL} {mode} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let exempt: Vec<String> = format!("{}\n{}", ask("--exemptions"), ask("--exemptions-derived"))
        .lines()
        .map(str::to_string)
        .collect();

    let unaccounted: Vec<String> = references
        .iter()
        .filter(|(_, ns, name)| {
            !declared.contains(&(ns.clone(), name.clone()))
                && !exempt.contains(&format!("ConfigMap/{ns}/{name}"))
                && !exempt.contains(&format!("ConfigMap/{name}"))
        })
        .map(|(at, ns, name)| format!("{at}: ConfigMap/{ns}/{name}"))
        .collect();
    assert!(
        unaccounted.is_empty(),
        "a manifest references a ConfigMap that no manifest declares and {DERIVE_REL} does not \
         exempt:\n    {}\n\
         Something generates it (or nothing does, and the pod will not start). A generated \
         object is undeclared as far as the orphan check can tell, so the first converge that \
         creates it closes FAILED at `check orphans` and never rolls the other instances — \
         train #916 did exactly this (backlog cb0c9937). Declare it in a manifest, or account \
         for it in {DERIVE_REL}: `derived_exemptions` when a script renders it per instance, \
         EXEMPT with the reason when it is built once.",
        unaccounted.join("\n    ")
    );
}

/// A line at column 0 that is not blank or a comment: the end of a
/// top-level YAML block.
fn top_level(line: &str) -> bool {
    !line.is_empty() && !line.starts_with(' ') && !line.starts_with('#')
}

// ---------------------------------------------------------------------------
// A manifest that will not parse. The defect this file was written for.
// ---------------------------------------------------------------------------

/// `svc-a.yaml` declares `Service/boss/svc-a` and the object is live, so
/// the tree and the cluster agree about it. Make the file unparseable
/// and its declaration drops out of the declared set — at which point a
/// DECLARED object reads as undeclared, and the verb that derives its
/// authority from this computation would delete it.
///
/// So the refusal has to be a refusal on all three counts: it names the
/// file, it says nothing about any object, and its exit code is the
/// cannot-answer one rather than the 0 that "clean" and "found an
/// orphan" both use.
#[test]
fn list_refuses_and_names_the_manifest_that_would_not_parse() {
    let c = Case::new("list-badparse", &[]);
    let bad = c.badparse("svc-a.yaml");
    let (rc, stdout, all) = c.run_env(&["--list"], &[bad]);
    assert_eq!(
        rc, CANNOT_ANSWER,
        "a partial scrape did not exit CANNOT_ANSWER ({CANNOT_ANSWER}):\n{all}"
    );
    assert_eq!(
        stdout, "",
        "the derivation reported an orphan set derived from a partial scrape:\n{all}"
    );
    assert!(
        !all.contains("svc-a\tboss")
            && !all.contains("0 undeclared")
            && !all.contains("1 undeclared"),
        "a partial scrape still produced a verdict about objects:\n{all}"
    );
    names_all(
        &all,
        &["svc-a.yaml", "cannot parse", "broken fixture"],
        "the unparseable-manifest case",
    );
}

/// `--declared` is the other half the lint reads, and it has the same
/// obligation: a declared set missing a file is not a smaller declared
/// set, it is no answer.
#[test]
fn declared_refuses_and_names_the_manifest_that_would_not_parse() {
    let c = Case::new("declared-badparse", &[]);
    let bad = c.badparse("deployment.yaml");
    let (rc, stdout, all) = c.run_env(&["--declared"], &[bad]);
    assert_eq!(
        rc, CANNOT_ANSWER,
        "--declared answered from a partial scrape:\n{all}"
    );
    assert_eq!(
        stdout, "",
        "--declared emitted a partial declared set:\n{all}"
    );
    names_all(
        &all,
        &["deployment.yaml", "cannot parse"],
        "the --declared unparseable case",
    );
}

/// The property a consumer needs and `-ne 0` cannot give it: in `--list`
/// a clean run and a run that found an orphan BOTH exit 0, so the only
/// way to read "I could not answer" is a code of its own. Pinned as one
/// comparison because the three codes are one contract.
#[test]
fn cannot_answer_is_distinguishable_from_clean_and_from_an_orphan() {
    let clean = Case::new("codes-clean", &[]).run(&["--list"]);
    let found = Case::new("codes-orphan", &[("Service", "boss", "ghost-svc", "")]).run(&["--list"]);
    let refused = {
        let c = Case::new("codes-refused", &[]);
        let bad = c.badparse("svc-b.yaml");
        c.run_env(&["--list"], &[bad])
    };
    assert_eq!(clean.0, 0, "clean:\n{}", clean.2);
    assert_eq!(found.0, 0, "found an orphan:\n{}", found.2);
    assert_ne!(
        refused.0, clean.0,
        "a refusal is indistinguishable from a clean run by exit code alone:\n{}",
        refused.2
    );
    assert_ne!(
        refused.0, found.0,
        "a refusal is indistinguishable from a finding by exit code alone:\n{}",
        refused.2
    );
    // And the two answers are told apart by stdout, which is why a
    // refusal must leave it empty.
    assert_eq!(clean.1, "");
    assert_eq!(found.1.trim(), "Service\tboss\tghost-svc");
    assert_eq!(refused.1, "");
}

// ---------------------------------------------------------------------------
// What a refusal must CARRY. A reason reduced to a count is the record
// thrown away before the reader sees it.
// ---------------------------------------------------------------------------

/// `--list` already declines to claim an unreadable pair clean, and says
/// UNVERIFIED with the pair named — the right shape. What it dropped was
/// the server's own words: the error text went to one scratch file that
/// the next pair overwrote, and nothing ever read it. Two unreadable
/// pairs must leave two reasons in the output, not a count.
#[test]
fn list_keeps_the_reason_each_unreadable_pair_gave() {
    let c = Case::new("unreadable", &[]);
    let forbid = c.forbid(&[("Service", "boss"), ("ConfigMap", "boss")]);
    let (rc, _stdout, all) = c.run_env(&["--list"], &[forbid]);
    assert_eq!(
        rc, 0,
        "a partly-readable cluster is an answer about the part it read:\n{all}"
    );
    names_all(
        &all,
        &[
            "UNVERIFIED",
            "Service in boss",
            "ConfigMap in boss",
            "cannot list Service in boss",
            "cannot list ConfigMap in boss",
        ],
        "the unreadable-pairs case",
    );
}

/// The last place a read error was swallowed whole. When an object is
/// not in the pair's listing the derivation asks about it directly, to
/// tell a controller-owned object from an absent one — and read a
/// FAILURE of that read as "it is not live". A credential that cannot
/// get the object says nothing about whether it exists, so this is
/// CANNOT ANSWER with the server's words, not a verdict.
#[test]
fn check_cannot_answer_when_the_object_itself_cannot_be_read() {
    let c = Case::new("forbidden-get", &[]);
    let deny = c.forbid_get("Service", "boss", "mystery-svc");
    let (rc, _stdout, all) = c.run_env(&["--check", "Service/boss/mystery-svc"], &[deny]);
    assert_eq!(
        rc, CANNOT_ANSWER,
        "an unreadable object was given a verdict:\n{all}"
    );
    names_all(
        &all,
        &["CANNOT ANSWER", "cannot get Service mystery-svc in boss"],
        "the unreadable-object case",
    );
    assert!(
        !all.contains("not live"),
        "'could not read it' was reported as 'it is not live':\n{all}"
    );
}

/// An object that really is absent keeps the verdict it had: NotFound is
/// a fact about the object, not about the credential. Here so the test
/// above cannot be satisfied by giving up on every failed read.
/// THE LAST MANIFEST OF A KIND IN A NAMESPACE. Measured 2026-09-12:
/// the seed-dir car deleted boss-dev's only CronJob manifest (#341);
/// the live CronJob stayed (apply does not prune) and failed every
/// minute against PodSecurity; `delete-orphan-object` REFUSED it —
/// "the tree declares no CronJob in boss-dev, so there is no declared
/// set to compare against" — and `--list` did not name it either. The
/// pair rule is right for Pods and ReplicaSets, which the tree never
/// declares anywhere; it is wrong for a kind the tree declares in
/// another namespace, on an object the tree itself labelled
/// `app.kubernetes.io/part-of=boss`. That label is the tree's own
/// claim, so the object is in scope by label where the pair is gone.
#[test]
fn a_labelled_object_of_a_kind_the_tree_declares_elsewhere_is_in_scope_by_label() {
    let c = Case::new("last-kind-labelled", &[]);
    // CronJob is declared in `boss` (boss-backup), never in `boss-dev`.
    c.add_live_labelled("CronJob", "boss-dev", "gate-seed-prepare", "boss");
    let (rc, stdout, all) = c.run(&["--list"]);
    assert_eq!(rc, 0, "{all}");
    assert!(
        stdout.contains("CronJob\tboss-dev\tgate-seed-prepare"),
        "the labelled orphan is named on stdout: {stdout}\n{all}"
    );
    let (rc, _, all) = c.run(&["--check", "CronJob/boss-dev/gate-seed-prepare"]);
    assert_eq!(rc, 0, "in scope by label, undeclared, no owner: {all}");
    assert!(
        all.contains("by label"),
        "the answer says WHY it is in scope: {all}"
    );
}

/// The same object without the tree's label is still refused — the
/// pair rule stands for everything the tree never marked as its own —
/// and the refusal now names the label as the way in.
#[test]
fn an_unlabelled_object_of_an_undeclared_pair_is_still_refused() {
    let c = Case::new("last-kind-unlabelled", &[]);
    c.add_live("CronJob", "boss-dev", "somebody-elses", "");
    let (rc, stdout, all) = c.run(&["--list"]);
    assert_eq!(rc, 0, "{all}");
    assert!(
        !stdout.contains("somebody-elses"),
        "unlabelled stays out of the list: {stdout}"
    );
    let (rc, _, all) = c.run(&["--check", "CronJob/boss-dev/somebody-elses"]);
    assert_eq!(rc, 3, "refused: {all}");
    assert!(
        all.contains("declares no CronJob in `boss-dev`") && all.contains("part-of"),
        "the refusal names both the missing pair and the label that would bring it in: {all}"
    );
}

/// A kind the tree declares NOWHERE stays out of scope even when
/// labelled: Pods and ReplicaSets carry the label too (a Deployment's
/// template propagates it), and they are the controller's, not the
/// tree's.
#[test]
fn a_labelled_object_of_a_kind_the_tree_never_declares_stays_out_of_scope() {
    let c = Case::new("last-kind-never-declared", &[]);
    c.add_live_labelled("Pod", "boss-dev", "boss-dev-abc12", "boss");
    let (rc, stdout, all) = c.run(&["--list"]);
    assert_eq!(rc, 0, "{all}");
    assert!(!stdout.contains("boss-dev-abc12"), "{stdout}");
    let (rc, _, all) = c.run(&["--check", "Pod/boss-dev/boss-dev-abc12"]);
    assert_eq!(rc, 3, "refused: a kind the tree declares nowhere: {all}");
}

/// The gate Job's manifest, in the shape `infra/gate-runner/gate-runner.yaml`
/// has: no name, a `generateName`, `boss-dev`, one literal label and two
/// placeholders `boss gate` fills at launch.
fn gate_template() -> String {
    r#"{"kind":"Job","metadata":{"generateName":"gate-$GATE_NAME_HINT-","namespace":"boss-dev","labels":{"app":"gate-runner","boss.dev/packet":"$GATE_RUN_JOB_ID","boss.dev/branch":"$GATE_NAME_HINT"}}}"#.to_string()
}

#[test]
fn owning_worker_isolation_must_be_present_before_either_reader_answers() {
    let c = Case::new("worker-source-isolation", &[]);
    let source: serde_json::Value = serde_json::from_slice(
        &std::fs::read(repo_root().join("infra/consist-worker/job.json")).unwrap(),
    )
    .unwrap();
    let template = c.root.join("worker.json");
    let empty = c.root.join("empty.json");
    std::fs::write(&empty, r#"{"items":[]}"#).unwrap();
    let reader = repo_root().join("infra/consist-worker/declaration.py");
    for (pointer, replacement) in [
        ("/spec/template/spec", Some(serde_json::json!([]))),
        (
            "/spec/template/spec/securityContext",
            Some(serde_json::Value::Null),
        ),
        (
            "/spec/template/spec/securityContext/seccompProfile",
            Some(serde_json::json!([])),
        ),
        (
            "/spec/template/spec/containers/0",
            Some(serde_json::Value::Null),
        ),
        (
            "/spec/template/spec/containers/0/securityContext",
            Some(serde_json::json!([])),
        ),
        (
            "/spec/template/spec/containers/0/securityContext/capabilities",
            Some(serde_json::Value::Null),
        ),
        (
            "/spec/template/spec/containers/0/securityContext/runAsUser",
            Some(serde_json::json!(true)),
        ),
        ("/spec/template/spec/automountServiceAccountToken", None),
        ("/spec/template/spec/securityContext", None),
        ("/spec/template/spec/containers/0/securityContext", None),
        ("/spec/template/spec/initContainers/0/securityContext", None),
        ("/spec/template/spec/hostPID", None),
        ("/spec/template/spec/hostIPC", None),
        ("/spec/template/spec/hostNetwork", None),
        ("/spec/template/spec/shareProcessNamespace", None),
        ("/spec/template/spec/hostPID", Some(serde_json::json!(true))),
        ("/spec/template/spec/hostIPC", Some(serde_json::json!(true))),
        (
            "/spec/template/spec/hostNetwork",
            Some(serde_json::json!(true)),
        ),
        (
            "/spec/template/spec/shareProcessNamespace",
            Some(serde_json::json!(true)),
        ),
        (
            "/spec/template/spec/automountServiceAccountToken",
            Some(serde_json::json!(true)),
        ),
        (
            "/spec/template/spec/securityContext/runAsNonRoot",
            Some(serde_json::json!(false)),
        ),
        (
            "/spec/template/spec/securityContext/fsGroup",
            Some(serde_json::json!(0)),
        ),
        (
            "/spec/template/spec/securityContext/seccompProfile/type",
            Some(serde_json::json!("Unconfined")),
        ),
        (
            "/spec/template/spec/containers/0/securityContext/allowPrivilegeEscalation",
            Some(serde_json::json!(true)),
        ),
        (
            "/spec/template/spec/containers/0/securityContext/readOnlyRootFilesystem",
            Some(serde_json::json!(false)),
        ),
        (
            "/spec/template/spec/containers/0/securityContext/runAsUser",
            Some(serde_json::json!(0)),
        ),
        (
            "/spec/template/spec/containers/0/securityContext/runAsGroup",
            Some(serde_json::json!(0)),
        ),
        (
            "/spec/template/spec/initContainers/0/securityContext/allowPrivilegeEscalation",
            Some(serde_json::json!(true)),
        ),
        (
            "/spec/template/spec/initContainers/0/securityContext/readOnlyRootFilesystem",
            Some(serde_json::json!(false)),
        ),
        (
            "/spec/template/spec/containers/0/securityContext/capabilities/drop",
            Some(serde_json::json!([])),
        ),
    ] {
        let mut malformed = source.clone();
        if let Some(value) = replacement {
            *malformed.pointer_mut(pointer).unwrap() = value;
        } else {
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            malformed
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(key);
        }
        std::fs::write(&template, serde_json::to_vec(&malformed).unwrap()).unwrap();
        for mode in ["--declaration", "--names"] {
            let out = Command::new("python3")
                .arg(&reader)
                .arg(mode)
                .arg(&template)
                .arg(&empty)
                .output()
                .unwrap();
            assert_eq!(
                out.status.code(),
                Some(4),
                "{pointer} {mode}: {}",
                String::from_utf8_lossy(&out.stdout)
            );
            assert!(
                out.stdout.is_empty(),
                "{pointer} {mode} returned a partial answer"
            );
            assert!(
                String::from_utf8_lossy(&out.stderr).contains("worker declaration cannot answer")
            );
        }
    }
    std::fs::write(&template, serde_json::to_vec(&source).unwrap()).unwrap();
    for mode in ["--declaration", "--names"] {
        let out = Command::new("python3")
            .arg(&reader)
            .arg(mode)
            .arg(&template)
            .arg(&empty)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn generated_workers_require_the_declared_shape_not_just_a_forged_app_label() {
    let c = Case::new("generated-worker-shape", &[]);
    let dir = c.tree.join("infra/consist-worker");
    std::fs::create_dir_all(&dir).unwrap();
    // The fixture uses the owning definition, rather than a second copy
    // of its mount/security roster. A controller-generated Job may gain
    // server defaults, but never new execution privileges or mounts.
    let template: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/consist-worker/job.json")).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("job.json"), serde_json::to_vec(&template).unwrap()).unwrap();
    let values = serde_json::json!({
        "$NAME": "consist-0123456789abcdef0123456789abcdef",
        "$IMAGE": "example.invalid/boss-ci:fixed",
        "$CLONE": "fixed historical clone script", "$WORKER": "fixed historical worker script",
        "$URL": "https://example.invalid/repo.git", "$REFERENCE": "refs/heads/test",
        "$HEAD": "0123456789abcdef0123456789abcdef01234567",
        "$BASELINE": "fedcba9876543210fedcba9876543210fedcba98",
        "$BUDGET": "120", "$DEADLINE": 300
    });
    fn fill(value: &mut serde_json::Value, bindings: &serde_json::Value) {
        match value {
            serde_json::Value::String(s) if bindings.get(s.as_str()).is_some() => {
                *value = bindings[s.as_str()].clone();
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(|v| fill(v, bindings)),
            serde_json::Value::Object(o) => o.values_mut().for_each(|v| fill(v, bindings)),
            _ => {}
        }
    }
    let mut good = template.clone();
    fill(&mut good, &values);
    let mut objects = vec![good.clone()];
    let mut defaulted = good.clone();
    defaulted["spec"]["template"]["spec"]
        .as_object_mut()
        .unwrap()
        .remove("hostPID");
    defaulted["spec"]["template"]["spec"]
        .as_object_mut()
        .unwrap()
        .remove("hostIPC");
    defaulted["spec"]["template"]["spec"]
        .as_object_mut()
        .unwrap()
        .remove("hostNetwork");
    defaulted["metadata"]["uid"] = serde_json::json!("336bc114-d132-44c1-8200-2b6ceef74a64");
    defaulted["spec"]["selector"] = serde_json::json!({"matchLabels":{"batch.kubernetes.io/controller-uid":"336bc114-d132-44c1-8200-2b6ceef74a64"}});
    defaulted["spec"]["template"]["metadata"]["labels"]["batch.kubernetes.io/controller-uid"] =
        serde_json::json!("336bc114-d132-44c1-8200-2b6ceef74a64");
    defaulted["spec"]["parallelism"] = serde_json::json!(1);
    defaulted["spec"]["template"]["spec"]["dnsPolicy"] = serde_json::json!("ClusterFirst");
    // A second representation of the same Job is a fixture control for
    // server defaults, not a second runtime object or creator claim.
    objects[0] = defaulted;
    for (i, key, value) in [
        (1, "hostPID", serde_json::json!(true)),
        (2, "automountServiceAccountToken", serde_json::json!(true)),
        (
            3,
            "containers",
            serde_json::json!([{"name":"forged","image":"evil"}]),
        ),
        (
            4,
            "volumes",
            serde_json::json!([{"name":"host","hostPath":{"path":"/"}}]),
        ),
    ] {
        let mut bad = good.clone();
        bad["metadata"]["name"] = serde_json::json!(format!("consist-{i:032x}"));
        bad["spec"]["template"]["spec"]["containers"][0]["command"][6] =
            bad["metadata"]["name"].clone();
        bad["spec"]["template"]["spec"][key] = value;
        objects.push(bad);
    }
    let mut unrelated = good.clone();
    unrelated["metadata"]["name"] = serde_json::json!("unrelated-same-app");
    unrelated["spec"]["template"]["spec"]["containers"][0]["command"][6] =
        unrelated["metadata"]["name"].clone();
    objects.push(unrelated);
    for (i, key, value) in [
        (
            5,
            "env",
            serde_json::json!([{"name":"INJECT","value":"yes"}]),
        ),
        (6, "command", serde_json::json!(["sh", "-c", "evil"])),
        (
            7,
            "volumeMounts",
            serde_json::json!([{"name":"forge-read","mountPath":"/etc/forge"}]),
        ),
        (8, "securityContext", serde_json::json!({"privileged":true})),
    ] {
        let mut bad = good.clone();
        bad["metadata"]["name"] = serde_json::json!(format!("consist-{i:032x}"));
        bad["spec"]["template"]["spec"]["containers"][0]["command"][6] =
            bad["metadata"]["name"].clone();
        bad["spec"]["template"]["spec"]["containers"][0][key] = value;
        objects.push(bad);
    }
    for object in &objects {
        c.add_live_labelled(
            "Job",
            "boss-dev",
            object["metadata"]["name"].as_str().unwrap(),
            "app=consist-worker",
        );
    }
    std::fs::write(
        c.live.join("Job.boss-dev.json"),
        serde_json::to_vec(&serde_json::json!({"items":objects})).unwrap(),
    )
    .unwrap();
    let (rc, stdout, all) = c.run(&["--list"]);
    assert_eq!(rc, 0, "{all}");
    assert!(
        !stdout.contains(values["$NAME"].as_str().unwrap()),
        "legitimate retained worker must be declared: {all}"
    );
    assert_eq!(
        stdout.lines().count(),
        9,
        "every hostile/unrelated labelled Job remains undeclared: {all}"
    );
    let (rc, _, all) = c.run(&[
        "--check",
        "Job/boss-dev/consist-0123456789abcdef0123456789abcdef",
    ]);
    assert_eq!(
        rc, 3,
        "declared worker cannot be an orphan-deletion target: {all}"
    );
    assert!(all.contains("infra/consist-worker/job.json"), "{all}");
    // A declaration reader cannot turn a malformed source into a
    // smaller declared set, which the deletion verb might act on.
    std::fs::write(dir.join("job.json"), r#"{"kind":"Job","metadata":{"name":"$NAME","namespace":"boss-dev","labels":{"app":"consist-worker"}}}"#).unwrap();
    let (rc, stdout, all) = c.run(&["--list"]);
    assert_eq!(
        rc, CANNOT_ANSWER,
        "partial owning declaration must refuse: {all}"
    );
    assert!(
        stdout.is_empty(),
        "no orphan answer may come from a broken declaration: {all}"
    );
}

/// Backlog 4438217e. `Job/boss-dev/seed-dir-probe-2` was made by hand on
/// 2026-09-12 — no owner, no ttlSecondsAfterFinished — and nothing could
/// ever name it: the tree's only Job in `boss-dev` is the gate runner's
/// generateName TEMPLATE, which declared no specific object and so put
/// no (Job, boss-dev) pair in scope. `--check` refused it with "the tree
/// declares no Job in boss-dev", and `delete-orphan-object`, which
/// derives its authority from this, could not retire it.
///
/// A template declares its KIND in its namespace, and the objects it
/// stamps out are the live ones carrying its literal labels. So the
/// hand-made Job is a finding and the gate Jobs — 285 of them live on
/// 2026-09-26, all `app=gate-runner` — are not; a CronJob's Job stays
/// out by its ownerReference, as before.
#[test]
fn a_generate_name_template_declares_its_kind_and_the_objects_that_carry_its_labels() {
    let c = Case::new("gate-template", &[]);
    // The control FIRST, on the same fixture: without the template, the
    // pair is out of scope and the hand-made Job is refused exactly as
    // the live sweep refused it.
    c.add_live("Job", "boss-dev", "seed-dir-probe-2", "");
    let (rc, stdout, all) = c.run(&["--check", "Job/boss-dev/seed-dir-probe-2"]);
    assert_eq!(rc, 3, "without a template there is no Job pair: {all}");
    assert!(
        all.contains("declares no Job in `boss-dev`"),
        "the refusal the packet measured: {all}"
    );
    assert_eq!(stdout, "");

    std::fs::write(
        c.tree.join("infra/gate-runner/gate-runner.yaml"),
        format!("{}\n", gate_template()),
    )
    .unwrap();
    for n in ["gate-fix-a-x1k2p", "gate-fix-b-7qz9m", "gate-train-c-0aa1b"] {
        c.add_live_labelled(
            "Job",
            "boss-dev",
            n,
            &format!("app=gate-runner,boss.dev/packet=p-{n},boss.dev/branch={n}"),
        );
    }
    c.add_live("Job", "boss-dev", "boss-playground-crawl-29311", "CronJob");

    let (rc, stdout, all) = c.run(&["--list"]);
    assert_eq!(rc, 0, "{all}");
    assert_eq!(
        stdout.trim(),
        "Job\tboss-dev\tseed-dir-probe-2",
        "the hand-made Job is the one finding; the gate Jobs and the CronJob's are not:\n{all}"
    );

    let (rc, stdout, all) = c.run(&["--check", "Job/boss-dev/seed-dir-probe-2"]);
    assert_eq!(rc, 0, "the verb's bound now names it: {all}");
    assert_eq!(stdout.trim(), "undeclared\tJob\tboss-dev\tseed-dir-probe-2");

    let (rc, _, all) = c.run(&["--check", "Job/boss-dev/gate-fix-a-x1k2p"]);
    assert_eq!(rc, 3, "a gate Job is declared by its template: {all}");
    names_all(
        &all,
        &["infra/gate-runner/gate-runner.yaml", "app=gate-runner"],
        "the template refusal names the file and the selector",
    );

    let (rc, _, all) = c.run(&["--check", "Job/boss-dev/boss-playground-crawl-29311"]);
    assert_eq!(rc, 3, "a CronJob's Job is its controller's: {all}");
    assert!(all.contains("ownerReferences[0].kind=CronJob"), "{all}");

    // A template declares its kind in ITS namespace only: `boss` holds
    // no Job pair, so a Job there is still refused, and the template
    // brings no label pair into scope either.
    c.add_live("Job", "boss", "someone-in-prod", "");
    let (rc, _, all) = c.run(&["--check", "Job/boss/someone-in-prod"]);
    assert_eq!(rc, 3, "{all}");
    assert!(all.contains("declares no Job in `boss`"), "{all}");
}

/// A template whose labels are ALL placeholders can match nothing it
/// stamped, so claiming its kind would make every live object of it a
/// finding. It declares nothing, as a nameless document did before.
#[test]
fn a_template_with_no_literal_label_declares_nothing() {
    let c = Case::new("gate-template-unlabelled", &[]);
    std::fs::write(
        c.tree.join("infra/gate-runner/gate-runner.yaml"),
        r#"{"kind":"Job","metadata":{"generateName":"gate-","namespace":"boss-dev","labels":{"boss.dev/packet":"$GATE_RUN_JOB_ID"}}}"#,
    )
    .unwrap();
    c.add_live("Job", "boss-dev", "gate-abcde", "");
    let (rc, stdout, all) = c.run(&["--list"]);
    assert_eq!(rc, 0, "{all}");
    assert_eq!(stdout, "", "{all}");
    let (rc, _, all) = c.run(&["--check", "Job/boss-dev/gate-abcde"]);
    assert_eq!(rc, 3, "{all}");
}

#[test]
fn check_still_says_not_live_for_an_object_that_is_absent() {
    let c = Case::new("absent", &[]);
    let (rc, _stdout, all) = c.run(&["--check", "Service/boss/boss-ghost"]);
    assert_eq!(rc, 3, "an absent object was not refused as absent:\n{all}");
    names_all(&all, &["boss-ghost", "not live"], "the absent case");
}

/// The line that fired once in five full-suite runs on 2026-09-19
/// (backlog 0f2ecbda, found by the builder of 2a056500 under load):
///
///   REFUSED Service/boss/boss-docs-internal — the tree does not own
///   namespace `boss` (no Namespace manifest in infra/cluster/manifests),
///     so nothing here can be called undeclared. Owned: boss boss-dev
///
/// `boss` refused as unowned in the line that lists it as owned. The
/// owned set was a newline-joined string and the membership test piped
/// it into `grep -q`; bash's printf wrote `boss` and `boss-dev` as two
/// writes, grep matched the first and exited, the second was SIGPIPE,
/// and pipefail turned a PRESENT member into "not found". It is now one
/// array and one loop, and this asks the derivation for every owned
/// namespace in the order the string was written — `boss` first, the
/// one whose match ended grep early — that an object it declares there
/// is refused as DECLARED and never as unowned. The shape itself is
/// refused, deterministically, by the producer-coin pin in
/// a_lint_that_cannot_read_does_not_say_clean.rs; this is the fact.
#[test]
fn an_owned_namespace_is_never_refused_as_unowned() {
    let c = Case::new("owned-is-owned", &[]);
    for (kind, ns, name) in [
        ("Service", "boss", "svc-a"),
        ("Service", "boss-dev", "boss-dev"),
    ] {
        let (rc, _stdout, all) = c.run(&["--check", &format!("{kind}/{ns}/{name}")]);
        assert_eq!(rc, 3, "a declared object was not refused:\n{all}");
        assert!(
            !all.contains("does not own namespace"),
            "the tree owns `{ns}` and the derivation said otherwise — the refusal that \
             read `Owned: boss boss-dev` and `does not own namespace boss` in one line \
             (backlog 0f2ecbda):\n{all}"
        );
        names_all(&all, &["DECLARES it"], "the declared case");
    }
    // The refusal a foreign namespace earns names the owned set, and the
    // set it names is the set it tested against.
    let (rc, _stdout, all) = c.run(&["--check", "Service/kube-system/kube-dns"]);
    assert_eq!(rc, 3, "a foreign namespace was not refused:\n{all}");
    names_all(
        &all,
        &[
            "does not own namespace `kube-system`",
            "Owned: boss boss-dev",
        ],
        "the foreign-namespace refusal",
    );
}

// ---------------------------------------------------------------------------
// The consumer. Moving the silence one layer up is not a fix.
// ---------------------------------------------------------------------------

/// The lint's path to the derivation, and the derivation's own path.
const LINT_REL: &str = "infra/lint/a-deleted-manifest-leaves-no-object.sh";
const DERIVE_REL: &str = "infra/cluster/undeclared-objects.sh";

/// The exemptions the DERIVATION holds, as `Kind/ns/name`. The lint
/// asserts none of them has gone stale, so the fixture's cluster has to
/// hold them — otherwise every run below fails for a reason no test here
/// is about.
const DERIVED_EXEMPTIONS: [(&str, &str, &str); 2] = [
    ("ConfigMap", "boss", "step-plugins"),
    ("ConfigMap", "boss-dev", "gate-runner-script"),
];

/// The lint, RUN out of a copy of the fixture tree: it reads the
/// manifests beside itself, calls the derivation beside itself, and walks
/// git history for the manifests the tree deleted, so the fixture has to
/// be a real repository. Both scripts are the repository's own, so every
/// verdict below is one the shipped code reached.
struct LintCase {
    c: Case,
    bin: PathBuf,
}

impl LintCase {
    fn new(name: &str, extra: &[(&str, &str, &str, &str)]) -> Self {
        let c = Case::new(name, extra);
        for (kind, ns, n) in DERIVED_EXEMPTIONS {
            c.add_live(kind, ns, n, "");
        }

        let bin = c.root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        // Not `std::fs::copy` + chmod: a copy holds the destination open
        // for writing in this process, which is the same race.
        let stub = std::fs::read_to_string(&c.kubectl).unwrap();
        write_exec(&bin.join("kubectl"), &stub);

        for rel in [LINT_REL, DERIVE_REL] {
            let dst = c.tree.join(rel);
            std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
            // The lint execs the derivation by path (`"$DERIVE" --list`),
            // so it is copied by the child writer, mode kept: a
            // `std::fs::copy` holds it open in this process (eed361e0).
            boss_testing::copy_exec(&repo_root().join(rel), &dst);
        }

        // A SEAM for the one case the real derivation cannot be driven
        // into from its inputs: `--list` refusing while `--declared`
        // answers. Both modes share the parse, so no fixture makes one
        // refuse and not the other — and what has to be shown is that the
        // lint consumes the CODE from the mode it asked, whichever mode
        // that is. Spliced in ahead of `set -uo pipefail`, hence
        // `${1:-}`; inert unless $STUB_LIST_REFUSES names a file.
        let derive = c.tree.join(DERIVE_REL);
        let body = std::fs::read_to_string(&derive).unwrap();
        let shebang = "#!/usr/bin/env bash\n";
        assert!(
            body.starts_with(shebang),
            "the derivation no longer opens with the shebang this seam is spliced after"
        );
        let guard = format!(
            "{shebang}if [ \"${{1:-}}\" = \"--list\" ] && [ -n \"${{STUB_LIST_REFUSES:-}}\" ]; then\n    \
             echo \"undeclared-objects: cannot parse ${{STUB_LIST_REFUSES}} — refusing to sweep against a declaration set that is missing it\" >&2\n    \
             exit 4\nfi\n"
        );
        // write_file, not std::fs::write: the path is already executable,
        // so it is rewritten in place by the child writer (eed361e0).
        boss_testing::write_file(&derive, &body.replacen(shebang, &guard, 1));

        let run_git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&c.tree)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run_git(&["init", "-q", "-b", "main"]);
        run_git(&["add", "-A"]);
        run_git(&["-c", "commit.gpgsign=false", "commit", "-qm", "fixture"]);

        LintCase { c, bin }
    }

    /// `(exit code, stdout+stderr)`. The lint's answer is its prose and
    /// its code; unlike the derivation it has no machine-read stdout.
    fn run(&self, extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(self.c.tree.join(LINT_REL))
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("STUB_LIVE", &self.c.live);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("the lint runs");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    /// Add one entry to the DERIVATION's exemption list — the only place
    /// an exemption can live. Used to make the two scripts disagree.
    fn exempt_in_derivation(&self, entry: &str) {
        let path = self.c.tree.join(DERIVE_REL);
        let body = std::fs::read_to_string(&path).unwrap();
        let anchor = "EXEMPT=(\n";
        assert!(
            body.contains(anchor),
            "the derivation's exemptions are no longer an `EXEMPT=(` array; rewrite this fixture"
        );
        std::fs::write(
            &path,
            body.replacen(anchor, &format!("{anchor}    \"{entry}\"\n"), 1),
        )
        .unwrap();
    }
}

/// `infra/lint/a-deleted-manifest-leaves-no-object.sh` is the other
/// reader of this question, and the one a human reads. Before the car
/// that collapsed the declared set it derived its own, and sent the
/// parse's stderr to /dev/null, so the fixture below made it print:
///
///     1 live object(s) in the managed namespaces that no manifest declares:
///         Service/svc-a (ns boss)
///
/// — a DECLARED object named as an orphan, under advice to `kubectl
/// delete` it, with no mention anywhere that a manifest would not parse.
/// Exit 1, the same code a genuine orphan produces.
#[test]
fn the_consuming_lint_refuses_rather_than_calling_a_declared_object_an_orphan() {
    let c = LintCase::new("lint", &[]);

    // The baseline, so the refusal below cannot be "it always fails".
    let (rc, out) = c.run(&[]);
    assert_eq!(rc, 0, "the lint did not pass a clean fixture:\n{out}");

    let (rc, out) = c.run(&[c.c.badparse("svc-a.yaml")]);
    assert_ne!(rc, 0, "the lint reported a partial scrape as clean:\n{out}");
    names_all(
        &out,
        &[
            "svc-a.yaml",
            "parse",
            // The derivation's OWN code, carried through. Reported as
            // `exit 0` by the first draft, which read `$?` inside the
            // `then` of an `if !` — where `!` has already made it 0.
            &format!("exit {CANNOT_ANSWER}"),
        ],
        "the lint's unparseable-manifest case",
    );
    assert!(
        !out.contains("Service/svc-a"),
        "the lint named a DECLARED object as an orphan because its manifest would not parse:\n{out}"
    );
}

/// A pair this credential cannot list is not a clean pair, and the lint
/// is the surface a human reads — so the server's own words have to reach
/// it. They come from the derivation's sweep now, which means the lint's
/// obligation is to pass them through rather than reduce them to a count
/// (CLAUDE.md §Diagnosis: quiet is a loan against the next diagnosis).
#[test]
fn the_lint_carries_the_reason_a_pair_it_could_not_read_gave() {
    let c = LintCase::new("lint-unreadable", &[]);
    let forbid = c.c.forbid(&[("Service", "boss")]);
    let (rc, out) = c.run(&[forbid]);
    assert_eq!(
        rc, 0,
        "a partly-readable cluster is still an answer about the part read:\n{out}"
    );
    names_all(
        &out,
        &["UNVERIFIED", "cannot list Service in boss"],
        "the lint's unreadable-pair case",
    );
}

/// THE COLLAPSE, asserted the only way a collapse can be: make the two
/// scripts DISAGREE and watch the lint follow the derivation.
///
/// Until this car the lint carried its own `$EXEMPT`, so an exemption
/// added to the derivation changed nothing about what the lint reported —
/// the two answers matched only while nobody edited either list, which is
/// precisely the §9a failure mode. A test that checked the answers
/// instead of the authority would pass against two copies that happen to
/// agree today, and go on passing the day they stop.
///
/// `ConfigMap/boss/generated-thing` is the real shape of this: an object
/// that IS declared, just not as YAML, which is what both live exemptions
/// are.
#[test]
fn the_lint_follows_the_derivations_exemptions_rather_than_its_own() {
    let c = LintCase::new(
        "lint-exemption",
        &[("ConfigMap", "boss", "generated-thing", "")],
    );

    // Exempt nowhere: an orphan to both, so the assertion below cannot be
    // satisfied by a lint that reports nothing at all.
    let (rc, out) = c.run(&[]);
    assert_ne!(
        rc, 0,
        "an undeclared live object was not reported at all:\n{out}"
    );
    assert!(
        out.contains("generated-thing"),
        "the baseline never named the object this exemption is about:\n{out}"
    );

    c.exempt_in_derivation("ConfigMap/boss/generated-thing");
    let (rc, out) = c.run(&[]);
    assert_eq!(
        rc, 0,
        "the lint did not follow the derivation's exemption — it is still deciding \
         'undeclared' for itself, which is the second definition backlog 19aa75e0 was \
         made of:\n{out}"
    );
    assert!(
        !out.contains("generated-thing"),
        "the lint named an object the derivation exempts:\n{out}"
    );
}

/// Clean, a finding, and a refusal to sweep — one test, because the three
/// are one contract and the only dangerous confusions are between them.
///
/// The refusal case holds a live orphan too. A lint that still swept the
/// cluster itself would name that orphan and never mention the file the
/// derivation refused over; a lint that read the refusal as "clean" would
/// exit 0. Both fail here, which is what "a refusal must not read as
/// clean and must not read as a finding" means in the only place it can
/// be measured.
#[test]
fn the_lint_tells_clean_from_an_orphan_from_a_refusal_to_sweep() {
    let clean = LintCase::new("lint-codes-clean", &[]).run(&[]);
    let found =
        LintCase::new("lint-codes-orphan", &[("Service", "boss", "ghost-svc", "")]).run(&[]);
    let refused = {
        let c = LintCase::new(
            "lint-codes-refused",
            &[("Service", "boss", "ghost-svc", "")],
        );
        c.run(&[("STUB_LIST_REFUSES", "ghost.yaml".to_string())])
    };

    assert_eq!(clean.0, 0, "a tree and a cluster that agree:\n{}", clean.1);
    names_all(
        &clean.1,
        // The derivation's own coverage line, carried through rather than
        // recomputed: a run that says nothing about how much of the scope
        // it covered is a green that means nothing.
        &["OK", "undeclared object(s) in"],
        "the lint's clean case",
    );

    assert_ne!(
        found.0, 0,
        "a live object no manifest declares was not reported:\n{}",
        found.1
    );
    names_all(
        &found.1,
        &["ghost-svc", "no manifest declares"],
        "the lint's orphan case",
    );

    assert_ne!(
        refused.0, 0,
        "a derivation that refused to sweep was reported as a clean cluster:\n{}",
        refused.1
    );
    names_all(
        &refused.1,
        &[
            "CANNOT ANSWER",
            // The file the refusal named, and the derivation's own code.
            "ghost.yaml",
            &format!("exit {CANNOT_ANSWER}"),
        ],
        "the lint's refused-sweep case",
    );
    assert!(
        !refused.1.contains("no manifest declares"),
        "a refusal was reported in the words of a finding about the cluster:\n{}",
        refused.1
    );
    assert!(
        !refused.1.contains("ghost-svc"),
        "the lint named an orphan from a sweep the derivation refused to run — so it \
         is sweeping the cluster itself:\n{}",
        refused.1
    );
}

/// The assertion that SURVIVED the collapse, and the one reason property
/// B shrank rather than vanished: the derivation does not check its own
/// exemptions for staleness, and a stale one silently excuses the next
/// object that takes the name. Both directions still fire, and both now
/// send the reader to the derivation rather than to this lint.
///
/// Here because "it shrank" would otherwise be indistinguishable from "it
/// went away quietly" — a deletion that takes a working check with it is
/// the failure this car is supposed to be the opposite of.
#[test]
fn the_lint_still_catches_a_stale_exemption_in_the_derivation() {
    let declared = LintCase::new("lint-stale-declared", &[]);
    // `svc-a.yaml` declares Service/boss/svc-a, so an exemption for it is
    // doing nothing and would excuse a future Service of that name.
    declared.exempt_in_derivation("Service/boss/svc-a");
    let (rc, out) = declared.run(&[]);
    assert_ne!(rc, 0, "an exemption the tree now declares passed:\n{out}");
    names_all(
        &out,
        &[
            "Service/boss/svc-a",
            "stale",
            "the tree now declares it",
            DERIVE_REL,
        ],
        "the stale-because-declared case",
    );

    let absent = LintCase::new("lint-stale-absent", &[]);
    absent.exempt_in_derivation("ConfigMap/boss/never-existed");
    let (rc, out) = absent.run(&[]);
    assert_ne!(
        rc, 0,
        "an exemption for an object the cluster does not have passed:\n{out}"
    );
    names_all(
        &out,
        &[
            "ConfigMap/boss/never-existed",
            "stale",
            "the cluster does not have it",
            DERIVE_REL,
        ],
        "the stale-because-absent case",
    );
}

/// THE CONVERGE'S PACKET NAMES THE OBJECT (backlog cb0c9937). The lint is
/// the converge's `check orphans` stage, and a finding used to leave the
/// packet `failed_stage: check orphans` and nothing more — which object
/// took an ops-request for the forge journal (packets 8d5c8f93, 79f23714).
/// The lint records `orphans_check` itself, through run-summary.sh, in the
/// file the unit names: the count and the names on a finding, bounded with
/// the remainder COUNTED, a stated ok on a pass — and nothing at all when
/// no packet asked (a gate, a hand run).
#[test]
fn the_lint_leaves_the_named_objects_for_the_converges_packet() {
    let with_summary = |name: &str, extra: &[(&str, &str, &str, &str)]| {
        let c = LintCase::new(name, extra);
        for rel in ["infra/run-summary.sh", "infra/lib/jq.sh"] {
            let dst = c.c.tree.join(rel);
            std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
            std::fs::write(&dst, std::fs::read(repo_root().join(rel)).unwrap()).unwrap();
        }
        c
    };
    let read = |file: &Path| -> serde_json::Value {
        let body = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("no run summary at {}: {e}", file.display()));
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("summary is not JSON ({e}): {body}"))
    };
    let two = [
        ("Service", "boss", "ghost-svc", ""),
        ("ConfigMap", "boss", "generated-thing", ""),
    ];

    // A finding: the count and both names.
    let c = with_summary("lint-summary-found", &two);
    let file = c.c.root.join("summary.json");
    let (rc, out) = c.run(&[("BOSS_RUN_SUMMARY_FILE", file.display().to_string())]);
    assert_ne!(rc, 0, "{out}");
    let got = read(&file)["orphans_check"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    names_all(
        &got,
        &[
            "2 finding(s)",
            "Service/boss/ghost-svc",
            "ConfigMap/boss/generated-thing",
        ],
        "the packet's orphans_check",
    );

    // Bounded: the names that do not fit are counted, never dropped in
    // silence — and the journal still has every one.
    let c = with_summary("lint-summary-bounded", &two);
    let file = c.c.root.join("summary.json");
    let (rc, out) = c.run(&[
        ("BOSS_RUN_SUMMARY_FILE", file.display().to_string()),
        ("BOSS_ORPHANS_SUMMARY_CAP", "70".to_string()),
    ]);
    assert_ne!(rc, 0, "{out}");
    let got = read(&file)["orphans_check"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    names_all(
        &got,
        &["2 finding(s)", "and 1 more"],
        "the bounded orphans_check",
    );
    assert_eq!(
        got.matches("no manifest declares it").count(),
        1,
        "the cap did not bound the names: {got}"
    );
    names_all(&out, &["ghost-svc", "generated-thing"], "the journal");

    // A pass says so, on the packet.
    let c = with_summary("lint-summary-clean", &[]);
    let file = c.c.root.join("summary.json");
    let (rc, out) = c.run(&[("BOSS_RUN_SUMMARY_FILE", file.display().to_string())]);
    assert_eq!(rc, 0, "{out}");
    let got = read(&file)["orphans_check"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(got.starts_with("ok"), "a clean run's orphans_check: {got}");

    // A refusal to sweep is not a finding and not a pass, there either.
    let c = with_summary("lint-summary-refused", &two);
    let file = c.c.root.join("summary.json");
    let (rc, out) = c.run(&[
        ("BOSS_RUN_SUMMARY_FILE", file.display().to_string()),
        ("STUB_LIST_REFUSES", "ghost.yaml".to_string()),
    ]);
    assert_ne!(rc, 0, "{out}");
    let got = read(&file)["orphans_check"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    names_all(
        &got,
        &["CANNOT ANSWER", "ghost.yaml"],
        "the refused orphans_check",
    );
    assert!(
        !got.contains("ghost-svc"),
        "a refusal named an object: {got}"
    );

    // No packet asked: nothing is written anywhere.
    let c = with_summary("lint-summary-unasked", &two);
    let (rc, out) = c.run(&[]);
    assert_ne!(rc, 0, "{out}");
    assert!(
        !c.c.root.join("summary.json").exists(),
        "a run outside a packet wrote a summary"
    );
}

/// A list nobody reads is the next thing to drift, and no RUN of the lint
/// can see one: an unread array is inert, not wrong. So this reads the
/// source — the one assertion in this file that does — and refuses the
/// DEFINITIONS whose second copy was the defect. Each has exactly one
/// home now, in the derivation, next to the set it modifies.
///
/// A definition, not a mention: the lint's header names all four to say
/// why they are not there and where they went, which is the documentation
/// a deletion owes its next reader, not a second copy.
#[test]
fn the_lint_keeps_no_second_copy_of_the_derivations_rules() {
    let body = std::fs::read_to_string(repo_root().join(LINT_REL)).expect("the lint is readable");
    for name in [
        "\nEXCLUDED_KINDS=",
        "\nEXEMPT_ANY_NS=",
        "\nEXEMPT=",
        "\nis_exempt()",
    ] {
        assert!(
            !body.contains(name),
            "{LINT_REL} defines `{}` again. It belongs to {DERIVE_REL}: two definitions \
             of what the tree does not declare differ in the places nobody compared, and \
             at the far end of delete-orphan-object that names a DECLARED object as \
             deletable (backlog 19aa75e0). Read the list from \
             `undeclared-objects.sh --exemptions` instead.",
            name.trim()
        );
    }
    assert!(
        body.contains("--list"),
        "{LINT_REL} no longer asks {DERIVE_REL} --list for the orphan set"
    );
}
