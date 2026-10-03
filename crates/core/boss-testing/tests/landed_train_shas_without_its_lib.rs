//! `infra/forge/landed-train-shas.lib.sh` without `infra/lib/secret-header.sh`
//! beside it declines its read ONLY where there is a machine token to
//! send (design 6805c764 car 4; review ef2da426 F5).
//!
//! Car 4 moved the lib's token onto `machine_token_header`, and a copy
//! of the lib that could not source the header lib declined every read,
//! token or not — a new way for the disk sweep's landed-train lookup to
//! say nothing, in a car whose title is "refuses nothing". Before car 4
//! a host with no token read unstamped, and it still does; a host that
//! HOLDS a token, and cannot hand it over in a file, declines (the
//! lookup then falls back to the age window: it prunes less, never more).

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};

const LIB: &str = "infra/forge/landed-train-shas.lib.sh";

const REPLY: &str = r#"{"total":1,"data":[
  {"status":"closed","steps":[{"metadata":{"train_ref":"train/20260911-1044@abc1234"}}]}
]}"#;

/// Source a lone copy of the lib (no infra/lib beside it) and ask it for
/// the landed shas; the keys it prints and what it said on stderr.
fn lone_lookup(tag: &str, token_dir: &std::path::Path) -> (String, String) {
    let root = scratch_dir(&format!("landed-train-shas-{tag}"));
    let forge = root.join("infra/forge");
    create_dir(&forge);
    write_file(
        &forge.join("landed-train-shas.lib.sh"),
        &std::fs::read_to_string(repo_root().join(LIB)).expect("the lib"),
    );
    write_file(&root.join("reply.json"), REPLY);
    let curl = root.join("curl");
    write_exec(&curl, "#!/bin/sh\ncat \"$STUB_REPLY\"\n");
    let driver = root.join("driver.sh");
    write_file(
        &driver,
        &format!(
            ". \"{}\"\nlanded_train_shas \"{}\" http://127.0.0.1:7900 120 test\n",
            forge.join("landed-train-shas.lib.sh").display(),
            curl.display()
        ),
    );
    let out = std::process::Command::new("bash")
        .arg(&driver)
        .env("STUB_REPLY", root.join("reply.json"))
        .env("BOSS_JOBS_URL", "http://127.0.0.1:7900")
        .env("BOSS_MACHINE_TOKEN_DIR", token_dir)
        .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
        .output()
        .expect("bash runs");
    assert!(out.status.success(), "{out:?}");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn no_token_reads_unstamped_as_before() {
    let none = scratch_dir("landed-train-shas-none").join("absent");
    let (keys, why) = lone_lookup("no-token", &none);
    assert!(
        keys.lines().any(|l| l == "abc1234"),
        "with no token there is nothing to hand over, so the read answers: {keys:?} / {why}"
    );
}

#[test]
fn a_token_it_cannot_hand_over_declines_the_read() {
    let mount = scratch_dir("landed-train-shas-mount").join("machine-token");
    create_dir(&mount);
    write_file(&mount.join("current"), "a-token-for-the-test\n");
    let (keys, why) = lone_lookup("token", &mount);
    assert!(keys.trim().is_empty(), "no answer: {keys:?}");
    assert!(
        why.contains("secret-header.sh") && why.contains("holds a token"),
        "the decline names the lib and the token: {why}"
    );
    assert!(
        !why.contains("a-token-for-the-test"),
        "never the value: {why}"
    );
}
