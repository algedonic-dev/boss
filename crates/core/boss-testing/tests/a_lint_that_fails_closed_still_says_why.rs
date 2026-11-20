//! A lint that fails closed must still say why.
//!
//! `grep -c` prints `0` and exits 1 when nothing matches, and under
//! `set -e` that kills the script even when the count is CAPTURED —
//! `n=$(grep -c ...)` takes grep's status as the assignment's status.
//!
//! MEASURED 2026-09-21 (backlog 4c9733f7). `the-sor-runs-on-control-plane`
//! originally counted its pins that way under `set -euo pipefail`. If the boss
//! manifest ever carried ZERO control-plane nodeSelectors — which is
//! exactly the condition the lint exists to catch, the one that took the
//! system of record dark for minutes on 2026-09-06 (90932d98) — the
//! script would die on the counting line and its seven-line diagnostic,
//! naming the invariant, the incident and the remedy, would never print.
//!
//! Failing closed is right. Failing SILENTLY is the defect: CLAUDE.md
//! §Diagnosis — a verdict must name what failed — and the whole value of
//! that lint is the explanation it could not reach in its own worst case.
//!
//! THE ASSERTION IS ON THE WORDS, not the exit code. A test that checked
//! only for non-zero would pass against the silent death, which is how
//! this survived in the first place.

use boss_testing::repo_root;
use std::path::Path;
use std::process::Command;

/// A fixture tree holding just what the lint reads: its own copy of the
/// script, the `scanned` helper it sources, and a manifest.
fn fixture(case: &str, manifest: &str) -> std::path::PathBuf {
    let root = boss_testing::scratch_dir(&format!("sor-control-plane-{case}"));
    let repo = repo_root();
    for dir in ["infra/lint/lib", "infra/cluster/manifests"] {
        std::fs::create_dir_all(root.join(dir)).expect("fixture dirs");
    }
    for f in [
        "infra/lint/the-sor-runs-on-control-plane.sh",
        "infra/lint/lib/scanned.sh",
        "infra/lint/lib/sor-placement.ts",
    ] {
        std::fs::copy(repo.join(f), root.join(f)).unwrap_or_else(|e| panic!("copy {f}: {e}"));
    }
    std::fs::write(root.join("infra/cluster/manifests/boss.yaml"), manifest).expect("manifest");
    root
}

fn run(root: &Path) -> (i32, String) {
    let out = Command::new("bash")
        .arg(root.join("infra/lint/the-sor-runs-on-control-plane.sh"))
        .output()
        .expect("the lint runs");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The worst case: workloads present, not one of them pinned.
#[test]
fn zero_pins_is_named_rather_than_dying_on_the_count() {
    let root = fixture("zero-pins", "kind: Deployment\n---\nkind: StatefulSet\n");
    let (code, text) = run(&root);
    assert_ne!(code, 0, "two unpinned workloads must not pass: {text}");
    assert!(
        text.contains("2 workload(s) but only 0"),
        "the lint must NAME the count it found, not die counting it: {text}"
    );
    assert!(
        text.contains("node-role.kubernetes.io/control-plane"),
        "and name the remedy: {text}"
    );
}

/// The control: a properly pinned manifest still passes. Without it the
/// test above is satisfied by a lint that refuses everything.
#[test]
fn a_pinned_manifest_still_passes() {
    let root = fixture(
        "pinned",
        "kind: Deployment\nspec:\n  template:\n    spec:\n      nodeSelector:\n        node-role.kubernetes.io/control-plane: \"\"\n---\nkind: StatefulSet\nspec:\n  template:\n    spec:\n      nodeSelector:\n        node-role.kubernetes.io/control-plane: \"\"\n",
    );
    let (code, text) = run(&root);
    assert_eq!(code, 0, "a pinned manifest is clean: {text}");
    assert!(text.contains("OK"), "{text}");
}

/// A manifest with no workloads at all counts zero on the OTHER `grep -c`
/// — the same trap one line up, and the one a fix aimed only at `pinned`
/// would leave behind.
#[test]
fn a_manifest_with_no_workloads_does_not_die_counting_them() {
    let root = fixture("empty", "# nothing here\n");
    let (code, text) = run(&root);
    assert!(
        text.contains("workload") || text.contains("scanned"),
        "it must say something about having found no workloads, not exit \
         wordlessly on the count: got code {code} and {text:?}"
    );
}

#[test]
fn the_approved_database_affinity_is_a_pin_not_an_unpinned_worker() {
    let root = fixture("database", &database_manifest("postgres", "boss", false));
    let (code, text) = run(&root);
    assert_eq!(
        code, 0,
        "the approved data-node placement is guarded: {text}"
    );
}

#[test]
fn the_database_exception_does_not_admit_other_workloads_or_optional_affinity() {
    for (name, namespace, optional) in [
        ("nats", "boss", false),
        ("postgres", "boss-playground", false),
        ("postgres", "boss", true),
    ] {
        let root = fixture(
            "database-refusal",
            &database_manifest(name, namespace, optional),
        );
        let (code, text) = run(&root);
        assert_ne!(code, 0, "{name}/{namespace} optional={optional}: {text}");
    }
    let required = database_manifest("postgres", "boss", false);
    let extra_term = format!(
        "{required}              - matchExpressions:\n                  - key: kubernetes.io/hostname\n                    operator: In\n                    values: [w-1]\n"
    );
    let wider_values = required.replace("values: [database]", "values: [database, build]");
    for manifest in [extra_term, wider_values] {
        let root = fixture("database-fallback", &manifest);
        let (code, text) = run(&root);
        assert_ne!(
            code, 0,
            "a build-node fallback is not a database pin: {text}"
        );
    }
}

fn database_manifest(name: &str, namespace: &str, optional: bool) -> String {
    let placement = if optional {
        "          preferredDuringSchedulingIgnoredDuringExecution:\n            - weight: 100\n              preference:\n                matchExpressions:\n                  - key: boss.dev/purpose\n                    operator: In\n                    values: [database]\n"
    } else {
        "          requiredDuringSchedulingIgnoredDuringExecution:\n            nodeSelectorTerms:\n              - matchExpressions:\n                  - key: boss.dev/purpose\n                    operator: In\n                    values: [database]\n"
    };
    format!(
        "kind: Namespace\nmetadata:\n  name: boss\n---\nkind: StatefulSet\nmetadata:\n  name: {name}\n  namespace: {namespace}\nspec:\n  template:\n    spec:\n      affinity:\n        nodeAffinity:\n{placement}"
    )
}
