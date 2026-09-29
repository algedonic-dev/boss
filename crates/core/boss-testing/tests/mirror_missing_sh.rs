//! `infra/forge/mirror-base-images.sh --missing` mirrors only the tags
//! the forge registry does not hold, and the converge runs it before
//! every image build.
//!
//! Measured 2026-09-13 01:20–03:20Z: car feat/the-conductor-can-launch-
//! a-gate added `COPY --from=10.20.0.15:3000/david/alpine-k8s:1.33.3`
//! to the cluster image. The mirror LIST had carried that tag since
//! 2026-09-09; the registry had never received it, because the mirror
//! runs only when a human files the verb. Six converges failed on
//! `not found`, two merged trains sat unconverged for two hours, and
//! the repair was one `mirror-base-images` ops-request. A tag in the
//! list is a declaration; the registry holding it is the fact, and the
//! converge is the place to close the gap — before the build that
//! needs it, against a stub-free `docker manifest inspect`.

use boss_testing::{repo_root, write_exec};
use std::process::Command;

/// A stub docker with a stub REGISTRY behind it, every call recorded.
///
/// - `pull` succeeds; `tag SRC DST` records that DST is SRC's image.
/// - `push DST` puts DST in the registry, at a digest derived from DST's
///   name — unless `STUB_PUSH_STALE` names DST, when the push exits 0
///   and the registry's TAG keeps serving an older manifest (the silent
///   no-op a push exit code cannot see).
/// - `manifest inspect [--insecure] REF` answers a tag the registry
///   holds (pushed, or listed in `present`) or a `repo@digest` it holds,
///   with that manifest; anything else is not found.
/// - `image inspect --format F REF`: `.Id` is the source image's id
///   (so a tag and its source agree), and `RepoDigests` lists the
///   source's own pull digest plus, for each registry tag pushed from
///   it, `repo@digest` — what docker records on push. `STUB_NO_REPO_DIGEST`
///   drops the pushed ones.
fn stub_docker(dir: &std::path::Path) -> std::path::PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exec(
        &bin.join("docker"),
        r#"#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_DIR/docker-calls"
touch "$STUB_DIR/tagged" "$STUB_DIR/pushed" "$STUB_DIR/present"
dig() { printf 'sha256:%s' "$(printf '%s' "$1" | sha256sum | cut -c1-64)"; }
manifest() { printf '{\n  "schemaVersion": 2,\n  "config": {\n    "digest": "%s"\n  }\n}\n' "$(dig "config-of-$1")"; }
ref="${!#}"
case "$1 $2" in
'manifest inspect')
    if grep -qxF "$ref" "$STUB_DIR/pushed" || grep -qxF "$ref" "$STUB_DIR/present"; then
        if [ "$ref" = "${STUB_PUSH_STALE:-}" ]; then manifest "an-older-$ref"; else manifest "$ref"; fi
        exit 0
    fi
    case "$ref" in
    *@sha256:*)
        repo="${ref%@*}"
        while read -r p; do
            [ "${p%:*}" = "$repo" ] && [ "$(dig "$p")" = "${ref#*@}" ] && { manifest "$p"; exit 0; }
        done < "$STUB_DIR/pushed"
        ;;
    esac
    echo "no such manifest: $ref" >&2
    exit 1 ;;
'image inspect')
    fmt="$4"
    src="$ref"
    while read -r s d; do [ "$d" = "$ref" ] && src="$s"; done < "$STUB_DIR/tagged"
    case "$fmt" in
    *RepoDigests*)
        echo "${src%:*}@$(dig "pulled-$src")"
        [ -n "${STUB_NO_REPO_DIGEST:-}" ] && exit 0
        while read -r s d; do
            [ "$s" = "$src" ] || continue
            if [ -n "${STUB_CONTAINERD:-}" ]; then
                # The containerd image store: every name sharing the
                # target lists the pulled INDEX digest, pushed or not —
                # and a single-platform push never sent that index.
                echo "${d%:*}@$(dig "index-of-$src")"
            elif grep -qxF "$d" "$STUB_DIR/pushed"; then
                echo "${d%:*}@$(dig "$d")"
            fi
        done < "$STUB_DIR/tagged"
        ;;
    *.Id*) dig "image-$src"; echo ;;
    esac
    exit 0 ;;
esac
case "$1" in
tag) echo "$2 $3" >> "$STUB_DIR/tagged" ;;
push) echo "$2" >> "$STUB_DIR/pushed" ;;
esac
exit 0
"#,
    );
    bin
}

/// Run the mirror with `args` over the stub; `(output, docker calls)`.
fn run_with(
    case: &str,
    args: &[&str],
    present: &[&str],
    extra: &[(&str, &str)],
) -> (std::process::Output, String) {
    let dir = boss_testing::scratch_dir(&format!("mirror-missing-{case}"));
    let bin = stub_docker(&dir);
    std::fs::write(dir.join("present"), present.join("\n") + "\n").unwrap();
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join("infra/forge/mirror-base-images.sh"))
        .args(args)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("STUB_DIR", &dir)
        .env("BOSS_FORGE_REGISTRY_BASE", "reg.test/david");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("bash runs the mirror script");
    (
        out,
        std::fs::read_to_string(dir.join("docker-calls")).unwrap_or_default(),
    )
}

fn run(case: &str, present: &[&str]) -> (std::process::Output, String) {
    run_with(case, &["--missing"], present, &[])
}

fn text(o: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Every forge tag the mirror's list names, read from `--check`.
fn listed() -> Vec<String> {
    let out = Command::new("bash")
        .arg(repo_root().join("infra/forge/mirror-base-images.sh"))
        .arg("--check")
        .env("BOSS_FORGE_REGISTRY_BASE", "reg.test/david")
        .output()
        .expect("--check runs");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split("->").nth(1))
        .map(|s| s.trim().to_string())
        .collect()
}

#[test]
fn missing_mirrors_only_the_tags_the_registry_lacks() {
    // Everything present except alpine-k8s, the tag that bit.
    let listed: Vec<String> = {
        let out = Command::new("bash")
            .arg(repo_root().join("infra/forge/mirror-base-images.sh"))
            .arg("--check")
            .env("BOSS_FORGE_REGISTRY_BASE", "reg.test/david")
            .output()
            .expect("--check runs");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split("->").nth(1))
            .map(|s| s.trim().to_string())
            .collect()
    };
    assert!(
        listed.iter().any(|t| t.ends_with("alpine-k8s:1.33.3")),
        "{listed:?}"
    );
    let present: Vec<&str> = listed
        .iter()
        .filter(|t| !t.ends_with("alpine-k8s:1.33.3"))
        .map(String::as_str)
        .collect();
    let (out, calls) = run("one-missing", &present);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pushes: Vec<&str> = calls.lines().filter(|l| l.starts_with("push ")).collect();
    assert_eq!(
        pushes,
        vec!["push reg.test/david/alpine-k8s:1.33.3"],
        "only the absent tag is pushed:\n{calls}"
    );
    assert!(
        calls
            .lines()
            .any(|l| l == "pull docker.io/alpine/k8s:1.33.3"),
        "the absent tag is pulled from its source:\n{calls}"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("1 image(s) mirrored") && text.contains("already in the registry"),
        "the log says what was done and what was already there:\n{text}"
    );
    // The presence check asks the plain-HTTP forge registry with
    // `--insecure`, as every other forge read does; without it the CLI
    // asks over HTTPS and reads every tag as absent (review of car E).
    assert!(
        calls
            .lines()
            .any(|l| l == "manifest inspect --insecure reg.test/david/bun:1.3-slim"),
        "the presence check does not pass --insecure:\n{calls}"
    );
    // The pushed tag was read back from the registry, by tag and by the
    // digest the pulled image records for it (backlog 1058e686, car E).
    assert!(
        calls
            .lines()
            .any(|l| l == "manifest inspect --insecure reg.test/david/alpine-k8s:1.33.3"),
        "the pushed tag is read back from the registry:\n{calls}"
    );
    if let Some(hits) = boss_testing::ops_runner_stub::effect_lines(VERB, &text) {
        assert_eq!(hits.len(), 1, "exactly the done line: {hits:?}\n{text}");
    }
}

#[test]
fn missing_with_everything_present_pulls_nothing() {
    let out = Command::new("bash")
        .arg(repo_root().join("infra/forge/mirror-base-images.sh"))
        .arg("--check")
        .env("BOSS_FORGE_REGISTRY_BASE", "reg.test/david")
        .output()
        .unwrap();
    let listed: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split("->").nth(1))
        .map(|s| s.trim().to_string())
        .collect();
    let present: Vec<&str> = listed.iter().map(String::as_str).collect();
    let (out, calls) = run("all-present", &present);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !calls.contains("pull ") && !calls.contains("push "),
        "{calls}"
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("0 image(s) mirrored"));
}

// ---------------------------------------------------------------------------
// THE PUSH IS READ BACK (backlog 1058e686, car E). `done` used to follow
// three exit codes — pull, tag, push — and nothing asked the registry
// what the tag now serves. Now every pushed tag is read back: the digest
// the PULLED image records for the forge repository (docker writes it
// when the push lands) must be a manifest the registry holds, and the
// forge TAG must serve that same manifest. Only then does `done` print.
// ---------------------------------------------------------------------------

const VERB: &str = "mirror-base-images";

/// The whole list, mirrored for real: every tag pushed is read back, and
/// the done line — the declared effect — says so.
#[test]
fn a_full_mirror_reads_every_pushed_tag_back() {
    let all = listed();
    let (out, calls) = run_with("full", &[], &[], &[]);
    let t = text(&out);
    assert!(out.status.success(), "{t}");
    for tag in &all {
        assert!(
            calls
                .lines()
                .any(|l| l == format!("manifest inspect --insecure {tag}")),
            "{tag} was not read back:\n{calls}"
        );
        assert!(
            t.contains(&format!("mirror-base-images: read back {tag} — ")),
            "{tag} has no read-back line:\n{t}"
        );
    }
    assert!(
        t.contains(&format!("done — {} image(s) mirrored", all.len())),
        "{t}"
    );
    if let Some(hits) = boss_testing::ops_runner_stub::effect_lines(VERB, &t) {
        assert_eq!(hits.len(), 1, "exactly the done line: {hits:?}\n{t}");
        assert!(hits[0].contains("done — "), "{hits:?}");
    }
}

/// A push that exits 0 while the registry's tag still serves an older
/// manifest is the silent no-op the exit code could not see: FAILED,
/// naming the tag, and no done line — and the images after it are still
/// pushed and read back, so one bad tag never strands the rest.
#[test]
fn a_tag_that_serves_another_manifest_fails_by_name() {
    let all = listed();
    let (out, calls) = run_with(
        "stale",
        &[],
        &[],
        &[("STUB_PUSH_STALE", "reg.test/david/bun:1.3-slim")],
    );
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "a stale tag passed:\n{t}");
    assert!(
        t.contains("FAILED") && t.contains("reg.test/david/bun:1.3-slim"),
        "{t}"
    );
    let pushes = calls.lines().filter(|l| l.starts_with("push ")).count();
    assert_eq!(
        pushes,
        all.len(),
        "the loop stopped at the stale tag:\n{calls}"
    );
    assert!(
        t.contains(
            "and 1 did not read back from the registry (each named above): reg.test/david/bun:1.3-slim"
        ),
        "the closing line names the one tag:\n{t}"
    );
    assert!(!t.contains("done — "), "no done line:\n{t}");
    if let Some(hits) = boss_testing::ops_runner_stub::effect_lines(VERB, &t) {
        assert!(hits.is_empty(), "an effect was claimed: {hits:?}\n{t}");
    }
}

/// The containerd image store (the default on a fresh Docker 29): every
/// name sharing a pulled image lists the pulled INDEX digest, which a
/// single-platform push never sends, so no read-back can find it in the
/// forge registry. That is a CANNOT ANSWER per image — and it must not
/// stop the loop, because the mirror is the tool that repairs a missing
/// base: every image is still pushed, and the run ends FAILED naming
/// each one (review of car E, run 7fe34bd7).
#[test]
fn an_index_digest_that_does_not_resolve_stops_no_image() {
    let all = listed();
    let (out, calls) = run_with("containerd", &[], &[], &[("STUB_CONTAINERD", "1")]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "an unread mirror passed:\n{t}");
    for tag in &all {
        assert!(
            calls.lines().any(|l| l == format!("push {tag}")),
            "{tag} was never pushed — the loop stopped early:\n{calls}"
        );
    }
    assert_eq!(
        t.matches("CANNOT ANSWER — pushed ").count(),
        all.len(),
        "one CANNOT ANSWER per image:\n{t}"
    );
    let closing = t
        .lines()
        .find(|l| l.contains("did not read back from the registry"))
        .unwrap_or_else(|| panic!("no closing FAILED line:\n{t}"));
    assert!(
        closing.contains(&format!("and {} did not read back", all.len())),
        "{closing}"
    );
    for tag in &all {
        assert!(closing.contains(tag.as_str()), "{tag} not named: {closing}");
    }
    assert!(!t.contains("done — "), "no done line:\n{t}");
}

/// A pulled image that records no digest for the forge repository after
/// the push cannot be compared with anything: CANNOT ANSWER, exit 1 —
/// never read as a match.
#[test]
fn no_recorded_digest_is_cannot_answer() {
    let (out, _) = run_with("no-digest", &[], &[], &[("STUB_NO_REPO_DIGEST", "1")]);
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");
    assert!(t.contains("CANNOT ANSWER"), "{t}");
    assert!(!t.contains("done — "), "no done line:\n{t}");
}

/// The verb declares an `effect`, and a `timeout` sized from the
/// script's own worst case: every listed image may spend its whole pull
/// retry backoff (`sleep $((attempt * 5))` for attempts 2..=max) before a
/// byte moves, and the bound must at least double that for the transfer
/// itself. Without one the runner kills it at 30 s — the rollback-to
/// finding (note_2026_09_29_timeouts).
#[test]
fn the_verb_declares_its_effect_and_a_timeout_past_its_worst_case() {
    let spec: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/mirror-base-images.json"))
            .unwrap(),
    )
    .unwrap();
    assert!(spec["effect"].as_str().is_some(), "{spec}");
    assert!(spec.get("effect_unread").is_none(), "{spec}");
    let script =
        std::fs::read_to_string(repo_root().join("infra/forge/mirror-base-images.sh")).unwrap();
    let max: u64 = script
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("local ref=\"$1\" attempt=1 max=")
                .and_then(|n| n.parse().ok())
        })
        .expect("pull_with_retry declares `local ref=\"$1\" attempt=1 max=<n>`");
    assert!(
        script.contains("sleep $((attempt * 5))"),
        "the backoff moved"
    );
    let backoff: u64 = (2..=max).map(|a| a * 5).sum();
    let images = listed().len() as u64;
    let floor = 2 * images * backoff;
    let timeout = spec["timeout"].as_u64().unwrap_or(0);
    assert!(
        timeout >= floor,
        "mirror-base-images declares timeout {timeout}; {images} images x {backoff} s of retry backoff, doubled, is {floor}"
    );
}

/// The converge calls it before the build, outside the lifted build
/// block, and a mirror that cannot complete does not stop the build —
/// the build names a base it lacks itself.
#[test]
fn the_converge_mirrors_what_is_missing_before_it_builds() {
    let src =
        std::fs::read_to_string(repo_root().join("infra/forge/cluster-deploy-runner.sh")).unwrap();
    let mirror = src
        .find("mirror-base-images.sh --missing")
        .expect("the runner runs mirror-base-images.sh --missing");
    let build = src.find("BUILD_FAILED_FILE=").expect("the build block");
    assert!(
        mirror < build,
        "the mirror step must come before the build block"
    );
}
