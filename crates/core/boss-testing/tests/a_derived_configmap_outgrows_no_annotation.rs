//! A ConfigMap the converge DERIVES from a tree directory is applied
//! server-side, so its size is bounded by the ConfigMap's own limit and
//! not by an annotation's — and when the apply is refused anyway, what
//! kubectl said reaches the converge's packet.
//!
//! MEASURED 2026-10-21 (backlog 73b14b64). Train #1312 let the
//! `step-plugins` loop take `infra/step-plugins/*.json` beside `*.js` and
//! added three plugin files: 253,710 bytes of data where #1311 had
//! 226,184. The stage piped `kubectl create … --dry-run=client -o yaml`
//! into a client-side `kubectl apply`, which stores the WHOLE object,
//! JSON-encoded (Go escapes `<`, `>` and `&` six bytes each), in
//! `metadata.annotations["kubectl.kubernetes.io/last-applied-configuration"]`
//! — and the API refuses any object whose annotations total more than
//! 262,144 bytes. The forge journal (ops-request journal-tail
//! cluster-deploy-runner.service, 2026-10-21) read: `The ConfigMap
//! "step-plugins" is invalid: metadata.annotations: Too long: may not be
//! more than 262144 bytes`. Every converge from 15:23Z failed at
//! `configmaps`, so no train reached the cluster — and the three
//! maintenance packets carried `failed_stage: configmaps` and nothing
//! else; the sentence above was in the journal alone.
//!
//! These tests run the runner's own `configmaps` stage text, extracted
//! from the file, against a `docker` on PATH that stands in for the
//! API server's one rule that matters here: a CLIENT-side apply whose
//! document exceeds 262,144 bytes is refused with that sentence; a
//! server-side apply stores no such annotation and is accepted.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

/// The API server's annotation limit (`TotalAnnotationSizeLimitB`).
const ANNOTATION_LIMIT: usize = 262_144;

/// `docker run … kubectl …`, as far as this stage uses it. `create
/// configmap` prints every file of the mounted plugin/gate directory
/// (the object's size is all the API rule below reads); `apply` refuses
/// a client-side document over the limit exactly as the API did, and
/// logs its argv so the test can say which apply ran.
const FAKE_DOCKER: &str = r#"#!/usr/bin/env bash
set -u
args="$*"
case "$args" in
*" create configmap "*)
    src=""
    prev=""
    for a in "$@"; do
        if [ "$prev" = "-v" ]; then
            case "$a" in
            *:/plugins:ro|*:/gate:ro) src="${a%%:*}" ;;
            esac
        fi
        prev="$a"
    done
    [ -n "$src" ] || { echo "fake docker: no source mount in: $args" >&2; exit 9; }
    cat "$src"/*
    ;;
*" apply "*)
    doc="$(cat)"
    printf '%s\n' "$args" >> "$FAKE_APPLY_LOG"
    if [ -n "${FAKE_APPLY_FAIL:-}" ]; then
        echo "$FAKE_APPLY_FAIL" >&2
        exit 1
    fi
    case "$args" in
    *--server-side*) echo "configmap/derived serverside-applied" ;;
    *)
        if [ "${#doc}" -gt "$FAKE_ANNOTATION_LIMIT" ]; then
            echo 'The ConfigMap "step-plugins" is invalid: metadata.annotations: Too long: may not be more than 262144 bytes' >&2
            exit 1
        fi
        echo "configmap/derived configured" ;;
    esac
    ;;
*) echo "fake docker: unexpected: $args" >&2; exit 9 ;;
esac
"#;

/// The runner's `configmaps` stage: from the `KP=` line that builds the
/// plugin kubectl to the comment that opens the roll, which is both
/// ConfigMaps this stage derives (step-plugins, gate-runner-script).
fn configmaps_stage(root: &Path) -> String {
    let script = fs::read_to_string(root.join("infra/forge/cluster-deploy-runner.sh")).unwrap();
    let start = script
        .find("KP=\"docker run")
        .expect("the runner builds the plugin kubectl as KP=\"docker run …");
    let end = script[start..]
        .find("\n# ONE patch, ONE revision.")
        .expect("the roll's comment closes the configmaps stage")
        + start;
    script[start..end].to_string()
}

struct Stage {
    tree: PathBuf,
    applies: PathBuf,
    summary: PathBuf,
}

/// A fixture tree: one plugin bundle of `plugin_bytes`, the three gate
/// runner keys, and a `docker` that is the fake above.
fn stage(name: &str, plugin_bytes: usize) -> Stage {
    let temp = boss_testing::scratch_dir(name);
    let tree = temp.join("tree");
    let plugins = tree.join("infra/step-plugins");
    let gate = tree.join("infra/gate-runner");
    fs::create_dir_all(&plugins).unwrap();
    fs::create_dir_all(&gate).unwrap();
    fs::write(plugins.join("big-renderer.js"), "x".repeat(plugin_bytes)).unwrap();
    fs::write(plugins.join("README.md"), "documentation, not a bundle").unwrap();
    for key in ["run.sh", "bootstrap.sh", "seed-refresh.sh"] {
        fs::write(gate.join(key), "#!/bin/sh\n").unwrap();
    }
    let bin = temp.join("bin");
    fs::create_dir_all(&bin).unwrap();
    boss_testing::write_exec(&bin.join("docker"), FAKE_DOCKER);
    Stage {
        tree,
        applies: temp.join("applies.log"),
        summary: temp.join("summary.json"),
    }
}

fn run(stage: &Stage, fail_with: Option<&str>) -> Output {
    let root = boss_testing::repo_root();
    let bin = stage.applies.parent().unwrap().join("bin");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    // The runner's own shell options, its own run-summary, and the
    // KAPPLY it defines above the stage — copied from the file rather
    // than retyped, so the -i the stdin pipe needs is the runner's.
    let script = fs::read_to_string(root.join("infra/forge/cluster-deploy-runner.sh")).unwrap();
    let kapply = script
        .lines()
        .find(|l| l.starts_with("KAPPLY=\""))
        .expect("the runner defines KAPPLY");
    let body = format!(
        "set -euo pipefail\n. \"$REAL_REPO/infra/run-summary.sh\"\n{kapply}\nSTAGE=\"configmaps\"\n{}\n",
        configmaps_stage(&root)
    );
    let mut cmd = Command::new("bash");
    cmd.env("PATH", path)
        .env("REAL_REPO", &root)
        .env("REPO", &stage.tree)
        .env("KUBECONFIG_PATH", "/nonexistent/kubeconfig")
        .env("SOURCE_NS", "boss")
        .env("BOSS_RUN_SUMMARY_FILE", &stage.summary)
        .env("FAKE_APPLY_LOG", &stage.applies)
        .env("FAKE_ANNOTATION_LIMIT", ANNOTATION_LIMIT.to_string())
        .env_remove("FAKE_APPLY_FAIL")
        .arg("-c")
        .arg(body);
    if let Some(msg) = fail_with {
        cmd.env("FAKE_APPLY_FAIL", msg);
    }
    cmd.output().unwrap()
}

#[test]
fn plugin_data_above_the_annotation_limit_still_converges() {
    let stage = stage("derived-configmap-above-limit", ANNOTATION_LIMIT + 40_000);
    let out = run(&stage, None);
    assert!(
        out.status.success(),
        "the configmaps stage refused {} bytes of plugin data — a client-side apply \
         stores the whole object in an annotation capped at {ANNOTATION_LIMIT}:\nstdout:\n{}\nstderr:\n{}",
        ANNOTATION_LIMIT + 40_000,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let applies = fs::read_to_string(&stage.applies).unwrap();
    let lines: Vec<&str> = applies.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "one apply per derived ConfigMap (step-plugins, gate-runner-script): {applies}"
    );
    for line in lines {
        assert!(
            line.contains(
                " apply --server-side --field-manager=cluster-deploy-runner --force-conflicts -f -"
            ),
            "a derived ConfigMap was applied client-side, which writes the \
             last-applied annotation: {line}"
        );
    }
}

#[test]
fn a_refused_apply_puts_what_kubectl_said_on_the_packet() {
    let stage = stage("derived-configmap-refused", 10);
    let said = "The ConfigMap \"step-plugins\" is invalid: a refusal only the journal used to hold";
    let out = run(&stage, Some(said));
    assert!(!out.status.success(), "a refused apply must fail the stage");
    let summary = fs::read_to_string(&stage.summary).unwrap_or_default();
    let summary: serde_json::Value = serde_json::from_str(&summary)
        .unwrap_or_else(|e| panic!("run summary is not JSON ({e}): {summary}"));
    let recorded = summary["failed_stage_output"].as_str().unwrap_or_else(|| {
        panic!("the refused apply left no failed_stage_output on the run summary: {summary}")
    });
    assert!(
        recorded.contains(said),
        "failed_stage_output must carry kubectl's own words verbatim: {recorded}"
    );
    assert!(
        recorded.contains("step-plugins"),
        "failed_stage_output must name which ConfigMap was refused: {recorded}"
    );
}
