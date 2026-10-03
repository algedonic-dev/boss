//! `infra/record-agent-runs.sh` names who files the records the way
//! every other reader does: `BOSS_ACTOR`, else the file
//! `BOSS_ACTOR_FILE` names, else `$HOME/.config/boss/actor` — and nobody
//! is a refusal.
//!
//! WHY (review 23f1c6fd N1, 2026-10-01). The dev pod's codex and gemini
//! sessions run with BOSS_ACTOR blank and BOSS_ACTOR_FILE=/dev/null, so
//! that they sign as nobody until registered rather than as the pod's
//! Claude, whose id is in /work/home/.config/boss/actor. identity.rs,
//! boss-api, hooks/lib.sh and open-page-audits.ts honour the override;
//! this script read the home file directly, so a codex session running
//! it filed records as claude@algedonic.dev.
//!
//! Pinned by running the script up to its filer check with a HOME whose
//! actor file names Claude. Nothing is sent: the refusal comes before
//! any request, and the URL points at a port nothing listens on.

use boss_testing::{create_dir, repo_root, scratch_dir, write_file};
use std::process::Command;

fn run(name: &str, env: &[(&str, &str)]) -> (i32, String) {
    let root = scratch_dir(&format!("record-agent-runs-{name}"));
    let home = root.join("home");
    create_dir(&home.join(".config/boss"));
    write_file(&home.join(".config/boss/actor"), "claude@algedonic.dev\n");
    let runs = root.join("runs.tsv");
    write_file(&runs, "# no runs\n");
    let mut cmd = Command::new(repo_root().join("infra/record-agent-runs.sh"));
    cmd.arg(&runs)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("BOSS_JOBS_URL", "http://127.0.0.1:9");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run record-agent-runs.sh");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn an_actor_file_override_pointing_at_nothing_files_as_nobody() {
    for (label, env) in [
        ("unset", vec![("BOSS_ACTOR_FILE", "/dev/null")]),
        (
            "blank",
            vec![("BOSS_ACTOR", ""), ("BOSS_ACTOR_FILE", "/dev/null")],
        ),
    ] {
        let (rc, out) = run(label, &env);
        assert_eq!(
            rc, 78,
            "{label}: BOSS_ACTOR_FILE=/dev/null names nobody, so the script must refuse \
             rather than file as the home file's Claude: {out}"
        );
        assert!(out.contains("no actor"), "{label}: {out}");
        assert!(!out.contains("claude@"), "{label}: {out}");
    }
}
