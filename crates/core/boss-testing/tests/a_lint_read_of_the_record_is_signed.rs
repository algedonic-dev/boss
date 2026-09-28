//! Every lint read of the system of record is SIGNED, as a reader.
//!
//! MEASURED 2026-09-27 ~12:15Z (backlog e76582c1). The guest-reads car
//! (5763e52e / 493cebf3, train #744 at 11:28Z) scoped the dispatcher's
//! rule read: a caller whose policy scope reads no packets — which is
//! what a request with no `x-boss-user` is — is answered 200
//! `{rules: [], error: "…withheld…"}`. `infra/lint/lib/sor-read.sh` sent
//! no header at all, so `the-live-rules-are-the-authored-rules` read the
//! withheld answer as ZERO enforced rules and failed every gate that ran
//! it, while a signed read of the same dispatcher answered 79 rules.
//!
//! Two pins, one per half of the defect:
//!
//! 1. Every lint that reads through `lint_sor_read` — the set is derived
//!    from the lint directory, never listed here — sends `x-boss-user`
//!    carrying the platform's read role (`audit-readonly`, which core
//!    policy grants Read at Scope::All and nothing else) at the auditor
//!    tier: the identity a recorded probe reads with through
//!    `boss-sor-read`. A lint judges the SYSTEM, so it must see the
//!    system, not the smaller world an anonymous caller is answered
//!    (memory: an unauthenticated read sees a smaller world).
//! 2. A rule read answered with a non-empty `error` fails NAMING that
//!    error, rather than counting the empty `rules` beside it as zero
//!    enforced rules — the failure then says why nothing was read.
//!
//! The lints run from the repo root against a stub `curl` on PATH that
//! records each call's URL and `x-boss-user` header, writes a canned
//! body to the `-o` file, and prints a canned HTTP code.
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The env var each lint's header names for its read surface. A lint
/// that reads through `lint_sor_read` and is absent here fails the
/// first test by name, so a new reader cannot go unpinned.
const URL_VARS: &[(&str, &str)] = &[
    (
        "the-live-protocols-are-the-authored-protocols.sh",
        "BOSS_JOBS_URL",
    ),
    (
        "the-live-rules-are-the-authored-rules.sh",
        "BOSS_DISPATCHER_URL",
    ),
    ("a-car-stays-under-the-edit-level.sh", "BOSS_JOBS_URL"),
];

/// Every lint under infra/lint/ (not lib/) that CALLS `lint_sor_read`.
fn sor_reading_lints() -> Vec<String> {
    let dir = repo_root().join("infra/lint");
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .expect("read infra/lint")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            std::fs::read_to_string(p)
                .map(|t| t.lines().any(|l| l.contains("$(lint_sor_read ")))
                .unwrap_or(false)
        })
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
        .collect();
    out.sort();
    out
}

struct Stub {
    bin: PathBuf,
    calls: PathBuf,
}

/// A `curl` that logs `<url>\t<x-boss-user value or NONE>` per call,
/// copies `body` into the `-o` file, and prints `STUB_CODE`. The
/// protocols lint's self-test child aims at `[::1]:9` on every run and
/// is refused and not logged, as in a_lint_that_reads_the_api_waits_out_a_roll.
fn stub(tag: &str, body: &str) -> Stub {
    let root = scratch_dir(&format!("lint-signed-{tag}"));
    let bin = root.join("bin");
    create_dir(&bin);
    let calls = root.join("curl-calls.txt");
    let body_file = root.join("body.json");
    write_file(&body_file, body);
    write_exec(
        &bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\n\
             case \"$*\" in *'[::1]:9'*) printf '000'; exit 7 ;; esac\n\
             out=''; user=NONE; url=''\n\
             while [ $# -gt 0 ]; do\n\
                 case \"$1\" in\n\
                     -o) out=$2; shift ;;\n\
                     -H) case \"$2\" in x-boss-user:*) user=${{2#x-boss-user: }} ;; esac; shift ;;\n\
                     -w|-m) shift ;;\n\
                     http*) url=$1 ;;\n\
                 esac\n\
                 shift\n\
             done\n\
             printf '%s\\t%s\\n' \"$url\" \"$user\" >> '{calls}'\n\
             [ -z \"$out\" ] || cp '{body}' \"$out\"\n\
             printf '%s' \"${{STUB_CODE:-503}}\"\n",
            calls = calls.display(),
            body = body_file.display(),
        ),
    );
    Stub { bin, calls }
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_lint(s: &Stub, lint: &str, url_var: &str, code: &str) -> Run {
    let root = repo_root();
    let out = Command::new("bash")
        .arg(root.join("infra/lint").join(lint))
        .current_dir(&root)
        .stdin(Stdio::null())
        .env(
            "PATH",
            format!(
                "{}:{}",
                s.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env(url_var, "http://registry.test:7900")
        .env("BOSS_SOR_WAIT_SECONDS", "0")
        .env("STUB_CODE", code)
        .env_remove("BOSS_ESTATE")
        .output()
        .unwrap_or_else(|e| panic!("run {lint}: {e}"));
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn calls(path: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            l.split_once('\t')
                .map(|(u, h)| (u.to_string(), h.to_string()))
        })
        .collect()
}

#[test]
fn every_lint_read_through_sor_read_is_signed_as_a_reader() {
    let lints = sor_reading_lints();
    assert!(
        !lints.is_empty(),
        "no lint under infra/lint calls lint_sor_read — the shape this test reads has changed"
    );
    for lint in &lints {
        let var = URL_VARS
            .iter()
            .find(|(l, _)| l == lint)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| {
                panic!(
                    "{lint} reads the record through lint_sor_read but is not in URL_VARS — \
                     name the env var its header gives for its read surface, so this pin runs it"
                )
            });
        let s = stub(lint.trim_end_matches(".sh"), "{}");
        let r = run_lint(&s, lint, var, "503");
        let seen = calls(&s.calls);
        assert!(
            !seen.is_empty(),
            "{lint}: never reached curl:\n{}\n{}",
            r.stdout,
            r.stderr
        );
        for (url, user) in seen {
            assert_ne!(
                user, "NONE",
                "{lint}: GET {url} went out UNSIGNED — an anonymous caller is answered a \
                 smaller world, and since train #744 the dispatcher withholds its rules \
                 from one (backlog e76582c1)"
            );
            let v: serde_json::Value = serde_json::from_str(&user)
                .unwrap_or_else(|e| panic!("{lint}: x-boss-user is not JSON ({e}): {user}"));
            assert_eq!(
                v["role"].as_str(),
                Some(boss_core::roles::AUDIT_READONLY_ROLE),
                "{lint}: a lint reads as the platform's READ role, never one that can write: {user}"
            );
            assert_eq!(
                v["access_tier"].as_str(),
                Some("auditor"),
                "{lint}: the reader's tier, as boss-sor-read's: {user}"
            );
            assert!(
                v["id"].as_str().is_some_and(|id| !id.is_empty()),
                "{lint}: the reader names who read: {user}"
            );
        }
    }
}

/// The measured answer: a 200 carrying `rules: []` and the withheld
/// reason. It must fail naming the reason — never as zero rules.
#[test]
fn a_withheld_rule_read_fails_naming_the_reason() {
    let reason = "this caller's policy scope reads no packets, so the rules of the machinery \
                  that moves them is withheld from it";
    let s = stub(
        "withheld",
        &serde_json::json!({ "rules": [], "error": reason }).to_string(),
    );
    let r = run_lint(
        &s,
        "the-live-rules-are-the-authored-rules.sh",
        "BOSS_DISPATCHER_URL",
        "200",
    );
    assert_eq!(r.code, 1, "{}\n{}", r.stdout, r.stderr);
    assert!(
        r.stderr.contains(reason),
        "the failure names the dispatcher's own reason:\n{}",
        r.stderr
    );
    assert!(
        !r.stderr.contains("ZERO enforced rules"),
        "a withheld read is not an empty registry:\n{}",
        r.stderr
    );
}
