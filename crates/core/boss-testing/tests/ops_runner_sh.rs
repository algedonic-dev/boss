//! `infra/ops/ops-runner.sh` is RUN, not read — against a stubbed
//! system of record (a `curl` on PATH that serves one open packet on
//! GET and records the completion's two writes — the keys through the
//! step merge door, then the status PUT), so every verdict below is one
//! the runner actually made.
//!
//! THE DEFECT (backlog 6964f9e8, measured 2026-09-08). The forge runner
//! refused ops-request 6d0c4e37 — `publish-github-pr --check`, the
//! verb's own no-network input check — with `verb publish-github-pr
//! takes at most 0 arg(s), got 1`. Two gaps: the allowlist had no way
//! to admit a bounded literal, so the only way to exercise the verb
//! was the real run; and the reason lived only in the forge journal —
//! the packet's step said `refused` and nothing else (CLAUDE.md
//! §Diagnosis: a verdict must name what failed).
//!
//! So: a param may be a `one_of` literal list (equality, never a
//! packet-supplied word in the argv), `optional` drops the placeholder
//! when the arg is absent, and every refusal writes `reason` on the
//! step — the same text the runner logs.
//!
//! The runner is sh + jq. A box without jq skips these with a line
//! saying so; the gate image has it.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::Command;

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// A scratch directory per case, so cases cannot see each other's
/// fixtures.
fn scratch(case: &str) -> PathBuf {
    // Per-uid and per-process, and it REFUSES by name if a
    // leftover cannot be cleared — see `boss_testing::scratch`.
    boss_testing::scratch_dir(&format!("ops-runner-sh-{case}"))
}

fn write_exec(path: &Path, body: &str) {
    boss_testing::write_exec(path, body);
}

/// The stubbed system of record: `bin/curl` serves `jobs.json` on any
/// GET (recording the URL it was asked for in `get.url`, so a case can
/// read the query the runner sent). The runner completes a step in two
/// writes (backlog 2aa2b19e, the gate runner's shape since e39a9d2a),
/// and the stub keeps each apart:
///   - the STEP MERGE DOOR (`PATCH .../steps/<id>/metadata`), the keys
///     the runner records on the step, is copied to `merge.json` and
///     appended to `STUB_MERGE_LOG` when set; it answers
///     `STUB_MERGE_CODE` (default 204) on `-w` — the FIRST merge
///     `STUB_MERGE_FIRST_CODE` instead when set (a roll met once) — and
///     writes `STUB_MERGE_BODY` to its `-o` file;
///   - the step PUT, the status alone, is copied to `put.json`; it
///     answers `STUB_PUT_CODE` (default 200) on `-w` and writes
///     `STUB_PUT_BODY` to its `-o` file — the server's refusal, which is
///     what a refused completion must carry. The FIRST PUT answers
///     `STUB_PUT_FIRST_CODE` / `STUB_PUT_FIRST_BODY` instead when set (a
///     race lost once), and every PUT body is appended to `STUB_PUT_LOG`
///     when set, so a case can count the sends. A code of `000` on
///     either door is no answer at all: the stub exits 7, as curl does
///     when it cannot connect;
///   - any other PATCH — the request-level door, which carries the queue
///     reading (1ffb3305) and a refused completion — is copied to
///     `patch.json` and appended to `STUB_PATCH_LOG` when set, because
///     one pass may write two.
///
/// Every REQUEST — a read as much as a write — is appended to
/// `STUB_CALL_LOG` when set, one line each: its method, its url, and
/// `token=yes` when one of its `-H @file` arguments holds the machine
/// token's header, `token=no` otherwise (backlog 44b2087e).
///
/// Every write's method and url is appended to `STUB_WRITE_ORDER` when
/// set, so a case can read which door was used first. A `gh` stub stands
/// in for the publish verb's `--check` tool probe, and a `df` stub for
/// the host's mounts.
fn stub_sor(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exec(
        &bin.join("curl"),
        "#!/bin/sh\n\
         m=GET; prev=; o=; w=; u=\n\
         for a in \"$@\"; do [ \"$prev\" = -X ] && m=\"$a\"; [ \"$prev\" = -o ] && o=\"$a\"; [ \"$prev\" = -w ] && w=1; case \"$a\" in http*) u=\"$a\";; esac; prev=\"$a\"; done\n\
         if [ -n \"${STUB_ARGV_LOG:-}\" ]; then printf '%s\\n' \"$@\" >> \"$STUB_ARGV_LOG\"; fi\n\
         if [ -n \"${STUB_CALL_LOG:-}\" ]; then t=no; q=; for a in \"$@\"; do if [ \"$q\" = -H ]; then case \"$a\" in @*) if grep -q '^x-boss-machine-token: ' \"${a#@}\" 2>/dev/null; then t=yes; fi;; esac; fi; q=\"$a\"; done; printf '%s %s token=%s\\n' \"$m\" \"$u\" \"$t\" >> \"$STUB_CALL_LOG\"; fi\n\
         p=\n\
         for a in \"$@\"; do if [ \"$p\" = -H ]; then p=\"$a\"; case \"$a\" in @*) if [ -n \"${STUB_HEADER_LOG:-}\" ]; then cat \"${a#@}\" >> \"$STUB_HEADER_LOG\"; fi;; esac; continue; fi; p=\"$a\"; case \"$a\" in @*)\n\
             if [ -n \"${STUB_WRITE_ORDER:-}\" ]; then printf '%s %s\\n' \"$m\" \"$u\" >> \"$STUB_WRITE_ORDER\"; fi\n\
             case \"$m $u\" in \"PATCH \"*/steps/*/metadata)\n\
                 cp \"${a#@}\" \"$STUB_MERGE\"\n\
                 if [ -n \"${STUB_MERGE_LOG:-}\" ]; then cat \"${a#@}\" >> \"$STUB_MERGE_LOG\"; echo >> \"$STUB_MERGE_LOG\"; fi\n\
                 mcode=\"${STUB_MERGE_CODE:-204}\"\n\
                 if [ -n \"${STUB_MERGE_FIRST_CODE:-}\" ] && [ ! -e \"$STUB_MERGE.first\" ]; then\n\
                     : > \"$STUB_MERGE.first\"; mcode=\"$STUB_MERGE_FIRST_CODE\"; fi\n\
                 if [ -n \"$o\" ]; then printf '%s' \"${STUB_MERGE_BODY:-}\" > \"$o\"; fi\n\
                 if [ -n \"$w\" ]; then printf '%s' \"$mcode\"; fi\n\
                 if [ \"$mcode\" = 000 ]; then echo 'curl: (7) Failed to connect' >&2; exit 7; fi\n\
                 exit 0;;\n\
             esac\n\
             if [ \"$m\" = PATCH ]; then cp \"${a#@}\" \"$STUB_PATCH\"\n\
                 if [ -n \"${STUB_PATCH_LOG:-}\" ]; then cat \"${a#@}\" >> \"$STUB_PATCH_LOG\"; echo >> \"$STUB_PATCH_LOG\"; fi\n\
             else cp \"${a#@}\" \"$STUB_PUT\"\n\
                 if [ -n \"${STUB_PUT_LOG:-}\" ]; then cat \"${a#@}\" >> \"$STUB_PUT_LOG\"; echo >> \"$STUB_PUT_LOG\"; fi\n\
                 code=\"${STUB_PUT_CODE:-200}\"; said=\"${STUB_PUT_BODY:-}\"\n\
                 if [ -n \"${STUB_PUT_FIRST_CODE:-}\" ] && [ ! -e \"$STUB_PUT.first\" ]; then\n\
                     : > \"$STUB_PUT.first\"; code=\"$STUB_PUT_FIRST_CODE\"; said=\"${STUB_PUT_FIRST_BODY:-}\"; fi\n\
                 if [ -n \"$o\" ]; then printf '%s' \"$said\" > \"$o\"; fi\n\
                 if [ -n \"$w\" ]; then printf '%s' \"$code\"; fi\n\
                 if [ \"$code\" = 000 ]; then echo 'curl: (7) Failed to connect' >&2; exit 7; fi\n\
             fi\n\
             exit 0;; esac; done\n\
         for a in \"$@\"; do case \"$a\" in http*) printf '%s\\n' \"$a\" > \"$STUB_GET\";; esac; done\n\
         cat \"$STUB_JOBS\"\n",
    );
    write_exec(&bin.join("gh"), "#!/bin/sh\nexit 0\n");
    // `df` is the host's too: the shipped `df` verb (and disk-report's
    // `df -k /`) would otherwise statfs every mount of the gate pod,
    // under the runner's 30 s default, on a node other gates are
    // loading — the wall time backlog 1ac5ee40 found a tree lint
    // losing. One fixed table, the shape `df -k /` is parsed in.
    write_exec(
        &bin.join("df"),
        "#!/bin/sh\nprintf 'Filesystem 1K-blocks Used Available Use%% Mounted on\\nstub 104857600 52428800 52428800 50%% /\\n'\n",
    );
    bin
}

/// One open ops-request for the named host, `execute` ready, carrying
/// the given verb and args. The runner acts only on an exact
/// `metadata.host` match, so this is also what decides whether a run
/// sees the packet at all.
fn packet_for(root: &Path, host: &str, verb: &str, args: &str) {
    std::fs::write(
        root.join("jobs.json"),
        format!(
            r#"{{"data":[{{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{{"host":"{host}","verb":"{verb}","args":{args}}},"steps":[{{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}],"total":1}}"#
        ),
    )
    .unwrap();
}

/// One open ops-request for the forge, `execute` ready, carrying the
/// given verb and args.
fn packet(root: &Path, verb: &str, args: &str) {
    packet_for(root, "forge", verb, args);
}

/// Run the runner once against the stub, with `verbs` as its allowlist
/// DIRECTORY (one file per verb — the shape the shipped
/// `infra/ops/verbs/` has). Returns (stdout+stderr, the keys the runner
/// recorded on the step through its merge door, if it wrote any).
fn run(
    root: &Path,
    verbs: &Path,
    extra_env: &[(&str, String)],
) -> (String, Option<serde_json::Value>) {
    run_from(&repo_root(), root, verbs, extra_env)
}

/// `run`, with the runner started from `checkout` — this tree, or a
/// checkout of its own (`runner_checkout`).
fn run_from(
    checkout: &Path,
    root: &Path,
    verbs: &Path,
    extra_env: &[(&str, String)],
) -> (String, Option<serde_json::Value>) {
    let put = root.join("put.json");
    let merge = root.join("merge.json");
    let _ = std::fs::remove_file(&put);
    let _ = std::fs::remove_file(&merge);
    let _ = std::fs::remove_file(root.join("merge.json.first"));
    let _ = std::fs::remove_file(root.join("patch.json"));
    let out = runner_from(checkout, root, verbs, extra_env)
        .output()
        .expect("ops-runner.sh runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let payload = std::fs::read_to_string(&merge)
        .ok()
        .map(|s| serde_json::from_str::<serde_json::Value>(&s).expect("merge payload is JSON"));
    (text, payload)
}

/// The runner, one pass, pointed at the stub with `verbs` as its
/// allowlist directory — not yet started, so a case can choose where its
/// output goes.
fn runner(root: &Path, verbs: &Path, extra_env: &[(&str, String)]) -> Command {
    runner_from(&repo_root(), root, verbs, extra_env)
}

/// `runner`, started from `checkout`: the runner derives its own
/// checkout from its `$0`, so this is the checkout its relative verb
/// scripts resolve against.
fn runner_from(
    checkout: &Path,
    root: &Path,
    verbs: &Path,
    extra_env: &[(&str, String)],
) -> Command {
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new("sh");
    cmd.arg(checkout.join("infra/ops/ops-runner.sh"))
        .env_clear()
        .env("PATH", path)
        .env("HOST_ID", "forge")
        .env("BOSS_JOBS_URL", "http://sor.invalid")
        // The machine token comes from the test, never from this host's
        // mount or rendered sor.env (design 6805c764 car 4).
        .env("BOSS_MACHINE_TOKEN_DIR", root.join("no-token-dir"))
        .env("BOSS_SOR_ENV", root.join("no-sor.env"))
        .env("OPS_VERBS_DIR", verbs)
        .env("STUB_JOBS", root.join("jobs.json"))
        .env("STUB_PUT", root.join("put.json"))
        .env("STUB_MERGE", root.join("merge.json"))
        .env("STUB_PATCH", root.join("patch.json"))
        .env("STUB_GET", root.join("get.url"));
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd
}

/// The real allowlist, verbatim: `infra/ops/verbs/*.json` copied file
/// by file. Its script paths are repo-relative and the runner resolves
/// them against its own checkout, so no rewriting is needed here
/// (66077f9c — this harness used to carry one of the four copies of
/// the `/home/david/boss/` substitution).
fn real_verbs(root: &Path) -> PathBuf {
    let dir = root.join("verbs");
    std::fs::create_dir_all(&dir).unwrap();
    for f in shipped_verb_files() {
        std::fs::copy(&f, dir.join(f.file_name().unwrap())).unwrap();
    }
    dir
}

/// A fixture allowlist: one file per (name, spec) under `verbs/`.
fn verbs_dir(root: &Path, specs: &[(&str, &str)]) -> PathBuf {
    let dir = root.join("verbs");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, spec) in specs {
        std::fs::write(dir.join(format!("{name}.json")), spec).unwrap();
    }
    dir
}

/// Every `*.json` under the shipped `infra/ops/verbs/`, sorted — the
/// directory IS the allowlist, so this is the definition every reader
/// is measured against.
fn shipped_verb_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(repo_root().join("infra/ops/verbs"))
        .expect("infra/ops/verbs/ exists")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files
}

/// What `publish-github-pr.sh --check` needs to say ok without a
/// network: a state dir and a bare repo standing in for the forge
/// checkout. No token: since backlog d2b7c947 the verb's token is the
/// GitHub App's, minted per request and rendered only by a run.
///
/// The stand-in carries a `main` commit, because since 2026-09-11
/// `--check` FETCHES `refs/heads/main` rather than only reading the
/// directory — a cheaper check passed while the publish failed.
fn publish_check_env(root: &Path) -> Vec<(&'static str, String)> {
    let state = root.join("state");
    let etc = root.join("etc");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&etc).unwrap();
    let forge = root.join("forge.git");
    let st = Command::new("git")
        .args(["init", "-q", "--bare", forge.to_str().unwrap()])
        .status()
        .expect("git runs");
    assert!(st.success());
    for args in [
        vec!["hash-object", "-t", "tree", "-w", "--stdin"],
        vec![
            "commit-tree",
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
            "-m",
            "seed",
        ],
    ] {
        let out = Command::new("git")
            .arg("-C")
            .arg(&forge)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if args[0] == "commit-tree" {
            let st = Command::new("git")
                .arg("-C")
                .arg(&forge)
                .args(["update-ref", "refs/heads/main", &sha])
                .status()
                .expect("git runs");
            assert!(st.success(), "update-ref refs/heads/main");
        }
    }
    // The forge's address file (/etc/boss/sor.env on the host), rendered
    // from the one source: the verb derives its forge clone URL from it.
    let sor_env = etc.join("sor.env");
    let rendered = Command::new("bash")
        .arg(repo_root().join("infra/estate/render-sor-env.sh"))
        .arg("--to")
        .arg(&sor_env)
        .output()
        .expect("render sor.env");
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    vec![
        ("BOSS_PUBLISH_STATE_DIR", state.display().to_string()),
        ("BOSS_FORGE_REPO_PATH", forge.display().to_string()),
        ("BOSS_SOR_ENV", sor_env.display().to_string()),
    ]
}

/// STALL EVERYTHING A TREE SCRIPT REACHES FOR. `bash`, `git`, `grep`,
/// `find` and `python3` on the case's PATH become stubs that note their
/// name in `tripped.log` and then hang for longer than the runner's
/// 30 s default — the shape a loaded gate node gave the real lint
/// (below). The runner and the libraries it sources call none of them,
/// so a case run under this answers exactly as fast as it would on an
/// idle box, and a verb that reaches past its fixture is named. Returns
/// the log's path.
fn stall_the_tree_tools(root: &Path) -> PathBuf {
    let log = root.join("tripped.log");
    for tool in ["bash", "git", "grep", "find", "python3"] {
        write_exec(
            &root.join("bin").join(tool),
            &format!(
                "#!/bin/sh\nprintf '%s %s\\n' {tool} \"$*\" >> '{}'\nexec sleep 45\n",
                log.display()
            ),
        );
    }
    log
}

macro_rules! needs_jq {
    () => {
        if !has("jq") {
            eprintln!("ops_runner_sh: SKIPPED — no jq on this box; the gate image has it");
            return;
        }
    };
}

/// A checkout of the runner's own under `root/checkout`: the runner, the
/// allowlist assembler and `infra/lib/` it sources, each LINKED from this
/// tree (so the code under test is this tree's, never a stale copy), and
/// nothing else. A script written there exists in no other checkout.
fn runner_checkout(root: &Path) -> PathBuf {
    use std::os::unix::fs::symlink;
    let checkout = root.join("checkout");
    std::fs::create_dir_all(checkout.join("infra/ops")).unwrap();
    for linked in [
        "infra/ops/ops-runner.sh",
        "infra/ops/verbs-allowlist.sh",
        "infra/lib",
    ] {
        symlink(repo_root().join(linked), checkout.join(linked))
            .unwrap_or_else(|e| panic!("link {linked} into the runner's checkout: {e}"));
    }
    checkout
}

/// A verb's script is named RELATIVE to the repo and resolved against
/// the runner's OWN checkout (backlog 66077f9c). Eleven of sixteen verbs
/// baked `/home/david/boss/infra/forge/…` — the forge checkout's path —
/// into argv[0], so none could run on boss-gcp (/opt/boss) and every
/// consumer (two lints, this harness) carried its own substitution of
/// that prefix: one path assumption in four places. The runner knows
/// where it is; a relative argv[0] resolves against that, an absent
/// script is a REFUSAL naming the resolved path, and a bare command is
/// left to PATH as before.
///
/// THE PROBE IS A FIXTURE, NOT A TREE LINT (backlog 1ac5ee40). Until
/// 2026-10-01 this case ran the real `infra/lint/no-manifest-mounts-a-
/// hostpath.sh` from this tree — bash, grep and find over the
/// manifests, 0.017 s alone — under the runner's 30 s default. Twice
/// that day, on a node other gates were building on, it was killed at
/// 30 s with 0 bytes written (`duration_ms: 30002, exit 124`): car gate
/// 001d7db3, and train gate eac9adf7, which cancelled a train of three
/// cars none of which touched it. The 30 s was inside the verb — the
/// runner's own record brackets only the exec, and its system of record
/// is the stub `curl`, so no SoR, DNS or retry budget was in reach. What
/// was in reach was the node: a real script's wall time is the gate
/// host's, and nothing about resolution needs it. So the runner runs
/// from a checkout of its own whose probe is a two-line `sh` script that
/// exists nowhere else, under `stall_the_tree_tools`: a probe that found
/// the script anywhere but the runner's checkout is refused, and one
/// that reaches for a tree tool is named instead of timing out.
#[test]
fn a_relative_argv0_resolves_against_the_runners_own_checkout() {
    needs_jq!();
    let root = scratch("relative-argv0");
    stub_sor(&root);
    let checkout = runner_checkout(&root);
    write_exec(
        &checkout.join("infra/ops/relative-argv0-probe.sh"),
        "#!/bin/sh\nprintf 'relative-argv0-probe: ran %s\\n' \"$0\"\n",
    );
    let verbs = verbs_dir(
        &root,
        &[
            (
                "probe",
                r#"{"about": "a script only the runner's own checkout carries", "hosts": ["forge"],
                    "argv": ["infra/ops/relative-argv0-probe.sh"], "params": []}"#,
            ),
            (
                "gone",
                r#"{"about": "a script this checkout does not carry", "hosts": ["forge"],
                    "argv": ["infra/ops/does-not-exist.sh"], "params": []}"#,
            ),
        ],
    );
    let tripped = stall_the_tree_tools(&root);
    packet(&root, "probe", "[]");
    let (out, payload) = run_from(&checkout, &root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    let reached = std::fs::read_to_string(&tripped).unwrap_or_default();
    assert!(
        reached.is_empty(),
        "the verb reached past its fixture into a tool this pass stalls:\n{reached}{md} / {out}"
    );
    assert_eq!(md["exit_code"], "0", "{md} / {out}");
    let ran = checkout.join("infra/ops/relative-argv0-probe.sh");
    assert!(
        md["output"]
            .as_str()
            .unwrap_or("")
            .contains(&format!("relative-argv0-probe: ran {}", ran.display())),
        "the relative script ran from the runner's own checkout: {md} / {out}"
    );

    packet(&root, "gone", "[]");
    let (out, payload) = run_from(&checkout, &root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md} / {out}");
    let reason = md["reason"].as_str().unwrap_or("");
    let gone = checkout.join("infra/ops/does-not-exist.sh");
    assert!(
        reason.contains(&gone.display().to_string()) && reason.contains("not in this checkout"),
        "the refusal names the resolved path: {md} / {out}"
    );

    // The shipped allowlist carries NO absolute checkout path any more.
    for f in shipped_verb_files() {
        let shipped = std::fs::read_to_string(&f).unwrap();
        assert!(
            !shipped.contains("/home/david/boss/"),
            "{} names a script by one host's checkout, not relative to the repo",
            f.display()
        );
    }
}

/// The allowed literal reaches the verb: `publish-github-pr --check` is
/// ANSWERED through the runner, and the verb's own check ran and said
/// ok. Before 6964f9e8 this exact packet was refused.
#[test]
fn the_allowed_literal_is_answered_through_the_runner() {
    needs_jq!();
    let root = scratch("literal-answered");
    stub_sor(&root);
    let verbs = real_verbs(&root);
    let env = publish_check_env(&root);
    packet(&root, "publish-github-pr", r#"["--check"]"#);
    let (out, payload) = run(&root, &verbs, &env);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert_eq!(md["exit_code"], "0", "{md} / {out}");
    assert!(
        md["output"].as_str().unwrap_or("").contains("--check ok"),
        "{md} / {out}"
    );
    assert!(out.contains("answered publish-github-pr"), "{out}");
}

/// A word outside the literal list is refused, and the refusal names
/// itself on the step: `reason` is present, equals `output`, names the
/// admitted literals, and is the SAME text the runner logs.
#[test]
fn a_word_outside_the_literal_list_is_refused_with_the_reason_on_the_step() {
    needs_jq!();
    let root = scratch("literal-refused");
    stub_sor(&root);
    let verbs = real_verbs(&root);
    packet(&root, "publish-github-pr", r#"["--force"]"#);
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md}");
    let reason = md["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("no reason on the step: {md}"));
    assert!(
        reason.contains("arg mode value --force is not one of --check"),
        "{reason}"
    );
    assert_eq!(md["reason"], md["output"], "reason and output differ: {md}");
    assert!(md["exit_code"].is_null(), "a refusal ran nothing: {md}");
    assert!(
        out.contains(&format!("refused aaaaaaaa — {reason}")),
        "the journal line must carry the same reason: {out}"
    );
}

/// Every refusal path writes through the one site: an unknown verb
/// and an over-long arg list each land their reason on the step too.
#[test]
fn every_refusal_path_names_its_reason_on_the_step() {
    needs_jq!();
    let root = scratch("refusal-paths");
    stub_sor(&root);
    let verbs = real_verbs(&root);

    packet(&root, "rm-rf", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md}");
    let reason = md["reason"].as_str().unwrap();
    assert!(
        reason.contains("verb rm-rf is not in the allowlist"),
        "{reason}"
    );
    assert!(
        out.contains(&format!("refused aaaaaaaa — {reason}")),
        "{out}"
    );

    packet(&root, "uptime", r#"["now"]"#);
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md}");
    let reason = md["reason"].as_str().unwrap();
    assert!(
        reason.contains("verb uptime takes at most 0 arg(s), got 1"),
        "{reason}"
    );
    assert!(
        out.contains(&format!("refused aaaaaaaa — {reason}")),
        "{out}"
    );
}

/// AN ARG IS ONE WORD, OR IT IS REFUSED (security review of fd7090cc,
/// 2026-09-24). jq's `test` runs Oniguruma in Perl syntax, where `$`
/// matches before a TRAILING NEWLINE — measured on jq 1.6:
/// `"target-a\n" | test("^[a-z-]{1,20}$")` is `true`. The argv is then
/// rebuilt from jq's output one line per word, so that newline became a
/// SECOND, empty word, and every placeholder after it moved one place —
/// on an approval verb, the place the signed plan's hash rides. So a
/// control character anywhere in an arg is refused by name before any
/// pattern is consulted, and every pattern must match the WHOLE string.
#[test]
fn an_arg_carrying_a_newline_or_a_control_character_is_refused() {
    needs_jq!();
    let root = scratch("control-chars");
    stub_sor(&root);
    let verbs = verbs_dir(
        &root,
        &[(
            "echo-word",
            r#"{"about":"test","hosts":["forge"],"argv":["echo","{1}","{2}"],
                "params":[{"name":"word","pattern":"^[a-z-]{1,20}$"},
                          {"name":"tail","pattern":"^[a-z]{1,5}$","optional":true}]}"#,
        )],
    );
    for (args, shown) in [
        (r#"["target-a\n"]"#, "U+000A"),
        (r#"["target-a\n","x"]"#, "U+000A"),
        (r#"["targ\u0001et"]"#, "U+0001"),
        (r#"["target-a\r"]"#, "U+000D"),
        (r#"["target-a\u007f"]"#, "U+007F"),
    ] {
        packet(&root, "echo-word", args);
        let (out, payload) = run(&root, &verbs, &[]);
        let md = payload.unwrap_or_else(|| panic!("{args}: no step completed: {out}"));
        assert_eq!(md["disposition"], "refused", "{args}: {md} / {out}");
        let reason = md["reason"].as_str().unwrap_or_default();
        assert!(
            reason.contains("control character") && reason.contains("word"),
            "{args}: the refusal names the arg and why: {reason}"
        );
        assert!(
            reason.contains(shown),
            "{args}: and names the character by code point, not raw: {reason}"
        );
        assert!(
            !reason.chars().any(|c| c == '\n' || c == '\u{1}'),
            "{args}: the reason itself carries no control character: {reason:?}"
        );
    }
    // And the clean word still runs.
    packet(&root, "echo-word", r#"["target-a"]"#);
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert_eq!(md["output"], "target-a\n", "{md}");
}

/// An optional literal that is absent DROPS its placeholder word: the
/// verb runs with no trailing empty argument (which `echo` would show
/// as a trailing space), and present it rides through verbatim.
#[test]
fn an_absent_optional_literal_drops_its_placeholder_word() {
    needs_jq!();
    let root = scratch("optional-omitted");
    stub_sor(&root);
    let verbs = verbs_dir(
        &root,
        &[(
            "say",
            r#"{"about":"echo","hosts":["forge"],"argv":["echo","ran","{1}"],"params":[{"name":"mode","one_of":["--check"],"optional":true}]}"#,
        )],
    );

    packet(&root, "say", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert_eq!(
        md["output"], "ran\n",
        "an omitted optional must not leave an empty word: {md}"
    );

    packet(&root, "say", r#"["--check"]"#);
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["output"], "ran --check\n", "{md}");
}

/// A VERB SERVES NAMED HOSTS. boss-gcp got a runner on 2026-09-11, and
/// 11 of the allowlist's 16 verbs name a script under the FORGE's
/// checkout (`/home/david/boss/infra/forge/...`), which does not exist
/// there. Unscoped, standing that runner up advertised a vocabulary of
/// which 11 could only fail on ENOENT — and an exec failure is not a
/// verdict (CLAUDE.md §Diagnosis). So a verb whose `hosts` does not
/// list this runner's HOST_ID is REFUSED, and the refusal names the
/// verb, this host, the hosts that verb does serve, and what this host
/// can be asked for instead.
#[test]
fn a_verb_that_does_not_serve_this_host_is_refused_by_name() {
    needs_jq!();
    let root = scratch("host-not-served");
    stub_sor(&root);
    let verbs = real_verbs(&root);
    packet_for(&root, "boss-gcp", "converge", "[]");
    let (out, payload) = run(&root, &verbs, &[("HOST_ID", "boss-gcp".to_string())]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md} / {out}");
    let reason = md["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("no reason on the step: {md}"));
    assert!(
        reason.contains("verb converge does not serve host boss-gcp"),
        "the refusal must name the verb and this host: {reason}"
    );
    assert!(
        reason.contains("scopes it to forge"),
        "the refusal must name the hosts the verb DOES serve: {reason}"
    );
    assert!(
        reason.contains("verbs this host serves: ") && reason.contains("df"),
        "the refusal must say what this host can be asked for instead: {reason}"
    );
    assert_eq!(md["reason"], md["output"], "reason and output differ: {md}");
    assert!(md["exit_code"].is_null(), "a refusal ran nothing: {md}");
    assert!(
        out.contains(&format!("refused aaaaaaaa — {reason}")),
        "the journal line must carry the same reason: {out}"
    );
}

/// And the five read-only, host-agnostic reads DO serve boss-gcp: the
/// bastion answers `df` through the same runner, with `runner_host`
/// recording which host answered.
#[test]
fn a_read_only_verb_answers_on_boss_gcp() {
    needs_jq!();
    let root = scratch("host-served");
    stub_sor(&root);
    let verbs = real_verbs(&root);
    packet_for(&root, "boss-gcp", "df", "[]");
    let (out, payload) = run(&root, &verbs, &[("HOST_ID", "boss-gcp".to_string())]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert_eq!(md["exit_code"], "0", "{md} / {out}");
    assert_eq!(md["runner_host"], "boss-gcp", "{md}");
    assert!(out.contains("answered df"), "{out}");
}

/// The mounted Secret's shape, under `root`: a directory whose `current`
/// file holds `token` — what the runner reads since design 6805c764 car 4
/// (infra/lib/secret-header.sh machine_token_header), in place of the
/// `BOSS_MACHINE_TOKEN` env var it read before.
fn token_dir(root: &Path, token: &str) -> String {
    let dir = root.join("machine-token");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("current"), format!("{token}\n")).unwrap();
    dir.display().to_string()
}

/// THE MACHINE TOKEN NEVER RIDES IN curl's ARGV (backlog 5f3ad356,
/// 2026-09-28). Every one of the runner's writes carried it as
/// `-H "x-boss-machine-token: <the token>"`, readable by every
/// local user in ps while curl ran. Under /bin/sh — dash on the hosts —
/// a whole pass now hands it over as `-H @<0600 file>`: the header still
/// reaches every request, the value is in no argv, and the private
/// directory is gone when the pass ends (its cleanup chained in front of
/// the runner's own EXIT trap).
#[test]
fn the_machine_token_rides_in_a_header_file_never_in_argv() {
    needs_jq!();
    let root = scratch("token-file");
    stub_sor(&root);
    let verbs = real_verbs(&root);
    packet_for(&root, "boss-gcp", "df", "[]");
    let tmp = root.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let argv_log = root.join("curl-argv.log");
    let header_log = root.join("curl-headers.log");
    let token = "stub-machine-token-5f3ad356";
    let (out, payload) = run(
        &root,
        &verbs,
        &[
            ("HOST_ID", "boss-gcp".to_string()),
            ("BOSS_MACHINE_TOKEN_DIR", token_dir(&root, token)),
            // The stub record is an estate host, as the runner's is.
            ("BOSS_MACHINE_TOKEN_HOSTS", "sor.invalid".to_string()),
            ("TMPDIR", tmp.display().to_string()),
            ("STUB_ARGV_LOG", argv_log.display().to_string()),
            ("STUB_HEADER_LOG", header_log.display().to_string()),
        ],
    );
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    let argv = std::fs::read_to_string(&argv_log).unwrap_or_default();
    assert!(!argv.is_empty(), "curl was never called: {out}");
    assert!(
        !argv.contains(token),
        "the machine token must never be in curl's argv:\n{argv}"
    );
    let headers = std::fs::read_to_string(&header_log).unwrap_or_default();
    assert!(
        headers.contains(&format!("x-boss-machine-token: {token}")),
        "the token must still reach the requests, as a header file:\n{headers}\n{out}"
    );
    let left: Vec<_> = std::fs::read_dir(&tmp)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    assert!(
        left.is_empty(),
        "the pass must leave nothing under TMPDIR: {left:?}"
    );
}

/// EVERY REQUEST OF A PASS CARRIES THE TOKEN, THE QUEUE READ FIRST
/// (backlog 44b2087e, 2026-10-07). The test above asks whether the token
/// reaches "the requests" and is satisfied by one of them: the runner's
/// seven writes handed curl the header file and its queue read — the
/// request it sends every minute on every host, whether or not anything
/// is queued — did not. Measured live: 77 tokenless `GET /api/jobs` from
/// `automation:ops-runner` in 44 minutes, every one a caller the jobs
/// gate would refuse under `enforce`, while its writes were clean. So
/// this reads the stub's own record of what it RECEIVED, request by
/// request, and holds every line of it.
///
/// And the other half of the rule (presented, never required): the same
/// pass with no token on the host answers the same request, and no
/// request carries a header.
#[test]
fn every_request_of_a_pass_carries_the_machine_token_and_the_queue_read_is_one() {
    needs_jq!();
    let token = "stub-machine-token-44b2087e";
    let pass = |case: &str, with_token: bool| -> (String, Vec<String>) {
        let root = scratch(case);
        stub_sor(&root);
        let verbs = real_verbs(&root);
        packet_for(&root, "boss-gcp", "df", "[]");
        let calls = root.join("curl-calls.log");
        let mut env = vec![
            ("HOST_ID", "boss-gcp".to_string()),
            ("BOSS_MACHINE_TOKEN_HOSTS", "sor.invalid".to_string()),
            ("STUB_CALL_LOG", calls.display().to_string()),
        ];
        if with_token {
            env.push(("BOSS_MACHINE_TOKEN_DIR", token_dir(&root, token)));
        }
        let (out, payload) = run(&root, &verbs, &env);
        let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
        assert_eq!(md["disposition"], "answered", "{md} / {out}");
        assert!(
            !out.contains(token),
            "the token is in the pass's output: {out}"
        );
        let lines = std::fs::read_to_string(&calls)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        (out, lines)
    };

    let (out, calls) = pass("token-every-request", true);
    assert!(
        calls
            .iter()
            .any(|l| l.starts_with("GET http://sor.invalid/api/jobs?kind=ops-request&status=open")),
        "the stub never received the queue read, so this case proves nothing about it:\n{}\n{out}",
        calls.join("\n")
    );
    assert!(
        calls.iter().any(|l| !l.starts_with("GET ")),
        "the pass wrote nothing, so this case holds the read alone:\n{}",
        calls.join("\n")
    );
    let bare: Vec<&String> = calls
        .iter()
        .filter(|l| !l.ends_with(" token=yes"))
        .collect();
    assert!(
        bare.is_empty(),
        "these requests reached the system of record without the machine token while the host \
         holds one — each is a caller its gate would refuse:\n{}\nall {} requests:\n{}",
        bare.iter()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
        calls.len(),
        calls.join("\n")
    );

    let (_, without) = pass("token-every-request-absent", false);
    assert_eq!(
        without.len(),
        calls.len(),
        "a host with no token sends the same requests:\n{}\nagainst\n{}",
        without.join("\n"),
        calls.join("\n")
    );
    assert!(
        without.iter().all(|l| l.ends_with(" token=no")),
        "no token on the host, and a request carried a header anyway:\n{}",
        without.join("\n")
    );
}

/// THE RUNNER PRESENTS ITS CREDENTIAL (design f623e425 Q1, option A;
/// backlog 6c9183de). The jobs API knows the runner by a credential it
/// presents in `x-boss-runner-credential`, never by the id it types, so
/// the runner reads its host's credential from a root-only FILE and
/// sends it beside the machine token on every request to the system of
/// record — as a header FILE, never in curl's argv, the machine token's
/// own rule. Two refusals the runner must NOT make (DR rule 62dac114,
/// no refusal on David's path): no file is a pass exactly as before,
/// and a file it cannot send is said on its log and the pass goes on
/// without it — the credential adds identity, it never stops an answer.
#[test]
fn the_runner_credential_rides_beside_the_machine_token_in_a_header_file() {
    needs_jq!();
    let root = scratch("runner-credential");
    stub_sor(&root);
    let verbs = real_verbs(&root);
    packet_for(&root, "boss-gcp", "df", "[]");
    let tmp = root.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let argv_log = root.join("curl-argv.log");
    let header_log = root.join("curl-headers.log");
    let token = "stub-machine-token-6c9183de";
    let credential = "stub-runner-credential-6c9183de-boss-gcp";
    let file = root.join("runner.credential");
    std::fs::write(&file, format!("{credential}\n")).unwrap();
    let env = |file: &Path| {
        vec![
            ("HOST_ID", "boss-gcp".to_string()),
            ("BOSS_MACHINE_TOKEN_DIR", token_dir(&root, token)),
            // The stub record is an estate host, as the runner's is.
            ("BOSS_MACHINE_TOKEN_HOSTS", "sor.invalid".to_string()),
            ("BOSS_RUNNER_CREDENTIAL_FILE", file.display().to_string()),
            ("TMPDIR", tmp.display().to_string()),
            ("STUB_ARGV_LOG", argv_log.display().to_string()),
            ("STUB_HEADER_LOG", header_log.display().to_string()),
        ]
    };
    let (out, payload) = run(&root, &verbs, &env(&file));
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    let argv = std::fs::read_to_string(&argv_log).unwrap_or_default();
    assert!(!argv.is_empty(), "curl was never called: {out}");
    assert!(
        !argv.contains(credential),
        "the runner credential must never be in curl's argv:\n{argv}"
    );
    assert!(!out.contains(credential), "nor on the runner's log:\n{out}");
    let headers = std::fs::read_to_string(&header_log).unwrap_or_default();
    let count = |line: &str| headers.lines().filter(|l| *l == line).count();
    let sent = count(&format!("x-boss-runner-credential: {credential}"));
    assert!(
        sent > 0,
        "the credential must reach the requests:\n{headers}\n{out}"
    );
    assert_eq!(
        sent,
        count(&format!("x-boss-machine-token: {token}")),
        "every request that carries the machine token carries the credential:\n{headers}"
    );

    // No file: the pass answers as it did before the door, and sends none.
    std::fs::remove_file(&header_log).unwrap();
    let (out, payload) = run(&root, &verbs, &env(&root.join("absent.credential")));
    let md = payload.unwrap_or_else(|| panic!("no step completed without a file: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    let headers = std::fs::read_to_string(&header_log).unwrap_or_default();
    assert!(!headers.contains("x-boss-runner-credential"), "{headers}");

    // A file that cannot be one header: said, and the pass still answers.
    std::fs::write(&file, format!("{credential}\nsecond-line\n")).unwrap();
    std::fs::remove_file(&header_log).unwrap();
    let (out, payload) = run(&root, &verbs, &env(&file));
    let md = payload.unwrap_or_else(|| panic!("no step completed with a bad file: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert!(
        out.contains("runner credential") && out.contains("NOT presented"),
        "a credential the runner cannot send is said on its log: {out}"
    );
    assert!(!out.contains(credential), "{out}");
    let headers = std::fs::read_to_string(&header_log).unwrap_or_default();
    assert!(!headers.contains("x-boss-runner-credential"), "{headers}");
    let left: Vec<_> = std::fs::read_dir(&tmp)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    assert!(
        left.is_empty(),
        "the pass must leave nothing under TMPDIR: {left:?}"
    );
}

/// THE RUNNER'S CREDENTIAL LEAVES ONLY FOR AN ESTATE HOST (backlog
/// 7369b078, F1 of review 9c484ca8). The credential's header was made by
/// plain `secret_header`, which judges no host, and since 44b2087e it
/// rides on the queue read the runner sends every minute — so with
/// `BOSS_JOBS_URL` pointed off the estate the machine token stayed home
/// and the runner's own credential went to whatever answered. It is now
/// made only when the machine token's host rule admits the record
/// (`machine_token_admits`, the same two functions `machine_token_header`
/// calls), and the file is not even read otherwise.
///
/// Read by EFFECT, from what the stub received: a record off the list
/// gets neither header and the pass still answers (DR rule 62dac114); the
/// rendered sor.env admits a host exactly as the env var does; and the
/// credential does not depend on the machine token being on the host.
#[test]
fn the_runner_credential_is_presented_only_to_a_host_the_machine_token_rule_admits() {
    needs_jq!();
    let token = "stub-machine-token-7369b078";
    let credential = "stub-runner-credential-7369b078-boss-gcp";
    // (case, BOSS_MACHINE_TOKEN_HOSTS, sor.env's list, a token on the host)
    let pass = |case: &str,
                env_hosts: Option<&str>,
                sor_hosts: Option<&str>,
                with_token: bool|
     -> (String, String) {
        let root = scratch(case);
        stub_sor(&root);
        let verbs = real_verbs(&root);
        packet_for(&root, "boss-gcp", "df", "[]");
        let header_log = root.join("curl-headers.log");
        let file = root.join("runner.credential");
        std::fs::write(&file, format!("{credential}\n")).unwrap();
        let mut env = vec![
            ("HOST_ID", "boss-gcp".to_string()),
            ("BOSS_RUNNER_CREDENTIAL_FILE", file.display().to_string()),
            ("STUB_HEADER_LOG", header_log.display().to_string()),
        ];
        if with_token {
            env.push(("BOSS_MACHINE_TOKEN_DIR", token_dir(&root, token)));
        }
        if let Some(hosts) = env_hosts {
            env.push(("BOSS_MACHINE_TOKEN_HOSTS", hosts.to_string()));
        }
        if let Some(hosts) = sor_hosts {
            let sor = root.join("sor.env");
            std::fs::write(&sor, format!("BOSS_MACHINE_TOKEN_HOSTS={hosts}\n")).unwrap();
            env.push(("BOSS_SOR_ENV", sor.display().to_string()));
        }
        let (out, payload) = run(&root, &verbs, &env);
        let md = payload.unwrap_or_else(|| panic!("[{case}] no step completed: {out}"));
        assert_eq!(md["disposition"], "answered", "[{case}] {md} / {out}");
        assert!(!out.contains(credential), "[{case}] on the log: {out}");
        (
            out,
            std::fs::read_to_string(&header_log).unwrap_or_default(),
        )
    };
    let sent = |headers: &str, name: &str| {
        headers
            .lines()
            .filter(|l| l.starts_with(&format!("{name}: ")))
            .count()
    };

    // The harness's record is http://sor.invalid. Off the list, three
    // ways: the env var names another host, the env var is set and empty
    // over a sor.env that would admit it, and no list anywhere.
    for (case, env_hosts, sor_hosts) in [
        ("rc-off-env", Some("elsewhere.invalid"), None),
        ("rc-off-empty-env", Some(""), Some("sor.invalid")),
        ("rc-off-no-list", None, None),
        ("rc-off-sor-env", None, Some("elsewhere.invalid")),
    ] {
        let (out, headers) = pass(case, env_hosts, sor_hosts, true);
        assert_eq!(
            sent(&headers, "x-boss-runner-credential"),
            0,
            "[{case}] the runner's credential left for a host the machine token's rule does not \
             admit:\n{headers}\n{out}"
        );
        assert_eq!(
            sent(&headers, "x-boss-machine-token"),
            0,
            "[{case}] {headers}"
        );
        assert!(
            out.contains("runner credential")
                && out.contains("NOT presented")
                && out.contains("sor.invalid")
                && out.contains("BOSS_MACHINE_TOKEN_HOSTS"),
            "[{case}] a withheld credential is said on the log, naming the host and the list: \
             {out}"
        );
    }

    // On the list, by the rendered sor.env alone: both ride, in step.
    let (out, headers) = pass("rc-on-sor-env", None, Some("sor.invalid"), true);
    let n = sent(&headers, "x-boss-runner-credential");
    assert!(
        n > 0,
        "an estate host is sent the credential:\n{headers}\n{out}"
    );
    assert_eq!(n, sent(&headers, "x-boss-machine-token"), "{headers}");
    assert!(!out.contains("NOT presented"), "{out}");

    // On the list with no machine token on the host: the credential is
    // judged by the host, not by whether a token happens to be mounted.
    let (out, headers) = pass("rc-on-no-token", Some("sor.invalid"), None, false);
    assert_eq!(
        sent(&headers, "x-boss-runner-credential"),
        n,
        "{headers}\n{out}"
    );
    assert_eq!(sent(&headers, "x-boss-machine-token"), 0, "{headers}");
}

/// `disk-report` answers on boss-gcp too (backlog d3c7eada, 2026-09-26):
/// the host's 48 GB root sat under its floor for nine days with only
/// `df` to read it, so nothing could say what fills it. Run through the
/// real allowlist with the host-shaped commands stubbed, it answers with
/// its verdict line rather than a refusal.
#[test]
fn disk_report_answers_on_boss_gcp() {
    needs_jq!();
    let root = scratch("disk-report-gcp");
    let bin = stub_sor(&root);
    for tool in ["sudo", "docker"] {
        write_exec(&bin.join(tool), "#!/bin/sh\nexit 1\n");
    }
    write_exec(
        &bin.join("du"),
        "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\nprintf '0\\t%s\\n' \"$last\"\n",
    );
    let verbs = real_verbs(&root);
    packet_for(&root, "boss-gcp", "disk-report", "[]");
    let (out, payload) = run(&root, &verbs, &[("HOST_ID", "boss-gcp".to_string())]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert_eq!(md["exit_code"], "0", "{md} / {out}");
    assert_eq!(md["runner_host"], "boss-gcp", "{md}");
    assert!(
        md["output"].as_str().unwrap_or("").contains("verdict: "),
        "the report must reach its verdict line: {md}"
    );
}

/// ABSENT MEANS REFUSE, so a verb cannot reach a host by forgetting to
/// say which hosts it serves — the fail-closed half, without which the
/// scoping would be advice rather than a rule.
#[test]
fn a_verb_declaring_no_hosts_is_refused_everywhere() {
    needs_jq!();
    let root = scratch("hosts-absent");
    stub_sor(&root);
    let verbs = verbs_dir(
        &root,
        &[(
            "say",
            r#"{"about":"echo","argv":["echo","ran"],"params":[]}"#,
        )],
    );

    packet(&root, "say", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(
        md["disposition"], "refused",
        "a verb with no hosts must be refused, not run: {md} / {out}"
    );
    let reason = md["reason"].as_str().unwrap();
    assert!(
        reason.contains("no host") && reason.contains("refused everywhere"),
        "the refusal must say the allowlist entry declares no hosts: {reason}"
    );
}

/// THE ALLOWLIST IS A DIRECTORY, one file per verb, and the verb's
/// name is its file name (backlog 5086842d). `infra/ops/verbs.json`
/// was one JSON object, and a JSON object has no uncontended insertion
/// point: on 2026-09-12 three cars each added a verb, two inserted
/// before the same key, and the conductor left one behind
/// (`conflict: infra/ops/verbs.json`) — the shape CLAUDE.md §9a records
/// for rules.toml before rules became one file each. Now adding a verb
/// is dropping a file in, touching no shared line.
///
/// The runner is the reader that matters most: it runs on two hosts
/// from their converged checkouts, and if it cannot load the directory
/// every ops verb dies. So this RUNS it over a directory and asks that
/// a verb be reachable by its file name, and that an unknown verb's
/// refusal lists every file — which is the runner saying, on the
/// packet, that it loaded them all.
#[test]
fn the_allowlist_is_the_directory_and_a_verb_is_named_by_its_file() {
    needs_jq!();
    let root = scratch("directory-allowlist");
    stub_sor(&root);
    let verbs = verbs_dir(
        &root,
        &[
            (
                "say-hello",
                r#"{"about":"echo","hosts":["forge"],"argv":["echo","hello"],"params":[]}"#,
            ),
            (
                "say-bye",
                r#"{"about":"echo","hosts":["forge"],"argv":["echo","bye"],"params":[]}"#,
            ),
        ],
    );
    // A README beside the verbs is prose, not a verb: the loader must
    // take only `*.json`.
    std::fs::write(verbs.join("README.md"), "# not a verb\n").unwrap();

    packet(&root, "say-bye", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert_eq!(md["output"], "bye\n", "{md}");

    packet(&root, "rm-rf", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md} / {out}");
    let reason = md["reason"].as_str().unwrap();
    assert!(
        reason.contains("infra/ops/verbs/") && reason.ends_with("verbs: say-bye, say-hello"),
        "the refusal names the directory and every verb file in it: {reason}"
    );
}

/// The SHIPPED directory loads whole: the runner's own listing of what
/// it knows equals the file names under `infra/ops/verbs/`. This is the
/// equality that makes the directory the definition — a verb file the
/// runner silently skipped would show up here as a name missing from
/// the refusal.
#[test]
fn the_runner_loads_every_shipped_verb_file() {
    needs_jq!();
    let root = scratch("shipped-directory");
    stub_sor(&root);
    let verbs = real_verbs(&root);
    // Sorted by NAME, the order the runner lists them in (jq's keys),
    // not by path: `prune-registry-versions-daily.json` sorts before
    // `prune-registry-versions.json` because `-` is below `.`, while the
    // name `prune-registry-versions` sorts first — the first pair of verb
    // names where one is a prefix of the other (backlog 8d77d670).
    let mut expected: Vec<String> = shipped_verb_files()
        .iter()
        .map(|f| f.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    expected.sort();
    assert!(expected.len() >= 16, "{expected:?}");
    assert!(
        !repo_root().join("infra/ops/verbs.json").exists(),
        "infra/ops/verbs.json is back — the directory is the allowlist now (5086842d); \
         a verb goes in infra/ops/verbs/<name>.json"
    );

    packet(&root, "rm-rf", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    let reason = md["reason"].as_str().unwrap();
    let listed = reason
        .rsplit("verbs: ")
        .next()
        .unwrap()
        .split(", ")
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        listed, expected,
        "the runner's allowlist is not the directory: {reason}"
    );
}

/// A directory the runner cannot load is a REFUSAL TO RUN naming the
/// directory (EX_CONFIG, the same exit an unreadable allowlist got),
/// never an empty allowlist that refuses every packet as unknown: no
/// directory, an empty one, and a file that is not a JSON object each
/// stop the runner before it touches a packet, and each says which.
#[test]
fn a_directory_the_runner_cannot_load_stops_it_by_name() {
    needs_jq!();
    let root = scratch("directory-broken");
    stub_sor(&root);
    packet(&root, "uptime", "[]");

    let missing = root.join("no-such-dir");
    let (out, payload) = run(&root, &missing, &[]);
    assert!(
        payload.is_none(),
        "a runner with no allowlist touched a packet: {out}"
    );
    assert!(
        out.contains("no-such-dir") && out.contains("refusing to run"),
        "{out}"
    );

    let empty = root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let (out, payload) = run(&root, &empty, &[]);
    assert!(payload.is_none(), "{out}");
    assert!(
        out.contains("holds no verb") && out.contains("empty"),
        "an empty directory must be named as the fault, not treated as an allowlist: {out}"
    );

    let broken = verbs_dir(
        &root,
        &[
            (
                "ok",
                r#"{"about":"echo","hosts":["forge"],"argv":["echo","ok"],"params":[]}"#,
            ),
            ("bad", "{not json"),
        ],
    );
    let (out, payload) = run(&root, &broken, &[]);
    assert!(payload.is_none(), "{out}");
    assert!(
        out.contains("bad.json"),
        "the fault must name the file that would not parse: {out}"
    );
}

/// A VERB THAT IS THE TREE'S OWN CLI SIGNS AS THE RUNNER. `run-car-probe`
/// runs `boss prove … --unattended` since backlog 9f00a805 (car 2): the
/// CLI signs every jobs-API call as `BOSS_ACTOR` and refuses a write
/// unnamed, so the runner hands its own account over in the verb's
/// environment — the same identity the step completion carries — and a
/// unit that set `BOSS_ACTOR` itself wins. Read back through a bare
/// command on PATH, the way `boss` resolves on the forge.
#[test]
fn a_cli_verb_signs_as_the_runners_own_account() {
    needs_jq!();
    let root = scratch("cli-verb-actor");
    let bin = stub_sor(&root);
    write_exec(
        &bin.join("who-signs"),
        "#!/bin/sh\nprintf 'signs-as=%s packet=%s\\n' \"${BOSS_ACTOR:-unset}\" \"${OPS_REQUEST_ID:-unset}\"\n",
    );
    let verbs = verbs_dir(
        &root,
        &[(
            "who-signs",
            r#"{"about": "prints the actor a CLI verb would sign as", "hosts": ["forge"],
                "argv": ["who-signs"], "params": []}"#,
        )],
    );
    packet(&root, "who-signs", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    let output = md["output"].as_str().unwrap_or("");
    assert!(
        output.contains("signs-as=automation:ops-runner"),
        "the verb must see the runner's own account as BOSS_ACTOR: {md} / {out}"
    );
    assert!(
        output.contains("packet=aaaaaaaa-0000-4000-8000-000000000000"),
        "the packet id still rides the environment: {md}"
    );

    // The runner's account is BOSS_OPS_ACTOR when a unit names one…
    let (out, payload) = run(
        &root,
        &verbs,
        &[("BOSS_OPS_ACTOR", "automation:forge-ops".into())],
    );
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert!(
        md["output"]
            .as_str()
            .unwrap_or("")
            .contains("signs-as=automation:forge-ops"),
        "{md}"
    );
    // …and an explicit BOSS_ACTOR on the unit outranks both.
    let (out, payload) = run(&root, &verbs, &[("BOSS_ACTOR", "emp-operator".into())]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert!(
        md["output"]
            .as_str()
            .unwrap_or("")
            .contains("signs-as=emp-operator"),
        "{md}"
    );

    // And the shipped verb itself is the CLI, not a script: a bare
    // `boss` on PATH, with the car first and both flags fixed words.
    let shipped: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/run-car-probe.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        shipped["argv"],
        serde_json::json!(["boss", "prove", "{1}", "--from-car", "--unattended"]),
        "run-car-probe's argv is the tree's CLI (9f00a805 car 2)"
    );
    assert!(
        !repo_root().join("infra/forge/run-car-probe.sh").exists(),
        "the shell twin was retired with 9f00a805 car 2; a script here is a second definition"
    );
}

/// THE VERB'S EXIT IS RECORDED ONCE, ON THE STEP (backlog 50fede8b).
/// It used to be recorded twice under two names — `exit_code` on the
/// execute step and `exit` on the request — written by one act and
/// held equal by nothing, which is the fact-that-lives-twice shape
/// CLAUDE.md §9a refuses. It was benign only while one writer wrote
/// both; a retry, a hand correction or a second runner writing one
/// without the other hands a reader a stale exit and a verdict on a
/// run that did not have it. Measured 2026-09-20 before the collapse:
/// every consumer already read the STEP — `boss ops --wait`'s verdict
/// line, `verb_failure` for the whole answered-ops-request judge
/// family, the yard's runner shed and signals — and no rule predicate,
/// handler or surface read the request-level copy. The request level
/// needs no copy to be readable, either: `GET /api/jobs?kind=ops-request`
/// returns each row WITH its steps, which is how the yard reads the
/// exit off `execute` from a list.
///
/// f47861a5's finding stands and is served by the step: a verb that
/// ran and failed must be visible above `answered` (publish-github-pr
/// printed `FAILED`, exited 1, and the publish step it was filed for
/// sat ready for five hours). What reads it is the
/// `complete-publish-pr-step-on-publish-github-pr-answered` rule, off
/// `exit_code`. The outcome stays `answered`: every judge rule keys on
/// it, and a verb that ran and said no is an answer, not a refusal.
#[test]
fn an_answered_verbs_exit_is_recorded_once_on_its_step() {
    needs_jq!();
    let root = scratch("exit-on-request");
    stub_sor(&root);
    let fails = root.join("fails.sh");
    write_exec(
        &fails,
        "#!/bin/sh\necho 'fails: step one ok'\necho 'fails: FAILED — the thing did not happen' >&2\nexit 3\n",
    );
    let verbs = verbs_dir(
        &root,
        &[(
            "fails",
            &format!(
                r#"{{"about":"a verb that fails","hosts":["forge"],"argv":["{}"],"params":[]}}"#,
                fails.display()
            ),
        )],
    );
    packet(&root, "fails", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert_eq!(md["exit_code"], "3", "{md} / {out}");
    let patch: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("patch.json"))
            .unwrap_or_else(|e| panic!("no PATCH on the request's metadata: {e}; {out}")),
    )
    .expect("the PATCH body is JSON");
    assert_eq!(
        // What is left on this door is the queue reading (1ffb3305) —
        // a request-level fact with no home on the step, unlike the
        // exit. This fixture's packet carries no `opened_at`, so it
        // has no `queued_s`. Nothing ELSE reaches the request's
        // metadata, and in particular no second spelling of the exit.
        patch,
        serde_json::json!({"queue_depth": 1}),
        "the request carries the queue it waited in and nothing else — the exit lives once, on the step: {patch}"
    );
    assert!(out.contains("answered fails"), "{out}");

    // A refusal ran nothing, so there is nothing to record about it
    // on the request at all.
    packet(&root, "not-a-verb", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md} / {out}");
    assert!(
        !root.join("patch.json").exists(),
        "a refusal writes nothing on the request: {out}"
    );
}

/// HOW LONG THE VERB TOOK, RECORDED WHERE ITS EXIT IS (backlog
/// b7bfe821). An answered request said THAT the verb exited, with what
/// code, and what it printed — and nothing about how long it ran. The
/// only duration derivable from the record was `opened_at` to
/// `closed_at`, which is dominated by up to 60 s of this runner's poll
/// latency and so cannot see a verb at all: the builder costing the
/// run-car-probe cadence on 2026-09-19 had to infer per-probe cost
/// from the SPREAD WITHIN a simultaneous batch — five packets filed at
/// 16:10:55 closing across 0.5 s — which is exactly the re-derivation
/// CLAUDE.md §Diagnosis refuses. The runner holds the number at the
/// moment it writes the exit and was dropping it, and that matters
/// most now: run-car-probe was 171 of the last 300 ops-requests and
/// its cadence went daily to hourly the same day, so the verb about to
/// dominate this serial walk had no per-run duration series to notice
/// a change in.
///
/// It rides the execute step, beside `exit_code` and `output`, because
/// it is a fact about the VERB'S RUN and not about the queue in front
/// of it — which is why `queue_depth` / `queued_s` ride the request
/// instead. One spelling, one writer, the 50fede8b rule above. A list
/// read returns each row with its steps, so the series this exists for
/// is one jobs-API query.
///
/// A refusal ran nothing, so it records no duration, the same way it
/// records no exit.
#[test]
fn an_answered_verbs_duration_is_recorded_on_its_step() {
    needs_jq!();
    let root = scratch("duration-on-step");
    stub_sor(&root);
    let slow = root.join("slow.sh");
    write_exec(&slow, "#!/bin/sh\nsleep 0.4\necho 'slow: done'\n");
    let verbs = verbs_dir(
        &root,
        &[(
            "slow",
            &format!(
                r#"{{"about":"a verb that takes a measurable moment","hosts":["forge"],"argv":["{}"],"params":[]}}"#,
                slow.display()
            ),
        )],
    );
    packet(&root, "slow", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    let ms = md["duration_ms"]
        .as_u64()
        .unwrap_or_else(|| panic!("no numeric duration_ms on the answered step: {md} / {out}"));
    assert!(
        (300..60_000).contains(&ms),
        "the recorded duration must be the verb's own run — it slept 0.4 s and the step says {ms} ms: {md} / {out}"
    );

    // A refusal ran nothing, so there is no duration to record — the
    // same posture as `exit_code`.
    packet(&root, "not-a-verb", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md} / {out}");
    // Sent as null, not left out: through the merge door an omitted key
    // keeps whatever an earlier answer left there, and one record must
    // not mix two passes (the re-review of car 59a6ade9).
    assert_eq!(
        md.get("duration_ms"),
        Some(&serde_json::Value::Null),
        "a refusal ran nothing to time, and clears any earlier timing: {md} / {out}"
    );
    assert_eq!(md.get("exit_code"), Some(&serde_json::Value::Null), "{md}");
}

/// A REQUEST IS LOGGED WHEN IT STARTS, not only when it ends (backlog
/// b81ff4ca). On 2026-09-28 the forge runner's journal read `answered
/// publish-github-pr` at 02:28:59 and then nothing for thirteen minutes
/// while eight requests queued; the item was filed as a silent runner
/// with three wrong hypotheses (a post-publish push, a lock, a blocked
/// unit), and only the NEXT line — `answered read-publish-checks on
/// 444d0f22 (exit 75, 2746B, 786285ms)`, printed when that run ended —
/// showed the runner had been busy the whole time. A line at the start
/// names what is running while it runs, so a silence is attributable
/// from the journal without re-deriving it (CLAUDE.md §Diagnosis).
///
/// The verb itself is the witness: it reads the runner's own output
/// WHILE it runs, so the line is proven to precede the run, not merely
/// to be somewhere in the pass's output.
#[test]
fn a_request_is_logged_when_it_starts_running() {
    needs_jq!();
    let root = scratch("logged-at-start");
    stub_sor(&root);
    let journal = root.join("journal.log");
    let witness = root.join("witness.sh");
    write_exec(
        &witness,
        "#!/bin/sh\n\
         if grep -q '^ops-runner: running witness on aaaaaaaa' \"$RUNNER_JOURNAL\"; then\n\
             echo 'witness: the start line was in the journal before I ran'\n\
         else\n\
             echo 'witness: NO start line in the journal while I ran'\n\
         fi\n",
    );
    let verbs = verbs_dir(
        &root,
        &[(
            "witness",
            &format!(
                r#"{{"about":"a verb that reads the runner's journal","hosts":["forge"],"argv":["{}"],"params":[],"timeout":45}}"#,
                witness.display()
            ),
        )],
    );
    packet(&root, "witness", "[]");
    let status = runner(
        &root,
        &verbs,
        &[("RUNNER_JOURNAL", journal.display().to_string())],
    )
    .stdout(std::fs::File::create(&journal).unwrap())
    .stderr(std::process::Stdio::null())
    .status()
    .expect("ops-runner.sh runs");
    let out = std::fs::read_to_string(&journal).unwrap_or_default();
    assert!(status.success(), "{out}");
    let md: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("merge.json"))
            .unwrap_or_else(|_| panic!("no step completed: {out}")),
    )
    .unwrap();
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert!(
        md["output"]
            .as_str()
            .unwrap_or("")
            .contains("the start line was in the journal before I ran"),
        "the start line must be written BEFORE the verb runs: {md} / {out}"
    );
    let start = out
        .lines()
        .find(|l| l.starts_with("ops-runner: running witness on aaaaaaaa"))
        .unwrap_or_else(|| panic!("no start line: {out}"));
    assert!(
        start.contains("timeout 45s"),
        "the start line says how long this request may hold the runner: {start}"
    );
    assert!(
        start.contains("queued"),
        "the start line says how long the request waited for the runner: {start}"
    );
    let (start_at, end_at) = (
        out.find("ops-runner: running witness on aaaaaaaa").unwrap(),
        out.find("ops-runner: answered witness on aaaaaaaa")
            .unwrap_or_else(|| panic!("no answered line: {out}")),
    );
    assert!(start_at < end_at, "started before answered: {out}");
}

/// A REFUSED COMPLETION SAYS WHY, ON THE REQUEST (post-mortem
/// 3c3b202c). On 2026-09-22 from 00:50 to 02:54 UTC both runners
/// stalled together, the forge's oldest request waiting 7394 s, and
/// nothing in the system of record named a cause. The cause: ops-request
/// v2 gated `execute` on `NOT requires_approval OR approve.done`, and
/// the step PUT's unresolved-blockers guard refused every completion
/// over the pending `approve` with a 409 (fixed in bd0f3369). The runner
/// ran each verb, met the 409, counted it `failed` and moved on, so it
/// re-ran every open request's verb once a minute for two hours. What
/// the record held about that: nothing. `curl -f` threw away the 409's
/// body, which named the blocker, so even the journal carried only the
/// status, and the unit going red was watched by nobody.
///
/// So a completion the server REFUSES (it answered, and not 2xx) is
/// written onto the request through the metadata door, which kept
/// working all night: the status, the server's own words, when, how
/// many times, and whether the verb ran anyway. That last one matters
/// because a refused answer re-runs the verb on the next pass. The
/// packet then says why it is stuck, and a queue alarm (a45b38c1) can
/// quote it rather than send someone to a journal.
/// A VERB WHOSE COMPLETION WAS REFUSED IS NEVER RUN AGAIN BY ITSELF
/// (backlog 865d37df, post-mortem 3c3b202c). The runner runs a verb
/// BEFORE its completion PUT, so a refused PUT left the step ready and
/// the next pass ran the verb again: on 2026-09-22 every open verb re-ran
/// about 120 times in two hours (58 cluster-converge packets in 66
/// minutes from one converge request). Those verbs were reads and
/// converges; ops-request v2 exists for destructive ones. So once the
/// request carries a refusal whose verb RAN, the runner holds it — skips
/// it, names the reason — and re-running is an explicit act: clearing
/// `completion_refused`. At-most-once matters more than completion.
#[test]
fn a_verb_whose_completion_was_refused_runs_exactly_once_across_passes() {
    needs_jq!();
    let root = scratch("completion-refused-held");
    stub_sor(&root);
    let ran = root.join("ran.count");
    let verb = root.join("verb.sh");
    write_exec(
        &verb,
        &format!("#!/bin/sh\necho x >> \"{}\"\necho did it\n", ran.display()),
    );
    let verbs = verbs_dir(
        &root,
        &[(
            "once",
            &format!(
                r#"{{"about":"a verb that must not repeat","hosts":["forge"],"argv":["{}"],"params":[]}}"#,
                verb.display()
            ),
        )],
    );
    let log = root.join("patches.jsonl");
    let env = vec![
        ("STUB_PUT_CODE", "409".to_string()),
        (
            "STUB_PUT_BODY",
            r#"{"error":"step has unresolved blockers"}"#.to_string(),
        ),
        ("STUB_PATCH_LOG", log.display().to_string()),
    ];
    let runs = || {
        std::fs::read_to_string(&ran)
            .unwrap_or_default()
            .lines()
            .count()
    };

    // Pass 1: the verb runs, the server refuses its completion, and the
    // refusal (verb_ran: true) is written onto the request.
    packet(&root, "once", "[]");
    let (out, _) = run(&root, &verbs, &env);
    assert_eq!(runs(), 1, "the first pass runs the verb: {out}");
    let written = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v.get("completion_refused").is_some())
        .unwrap_or_else(|| panic!("no refusal written: {out}"));
    assert_eq!(written["completion_refused"]["verb_ran"], true, "{written}");

    // Passes 2 and 3 see the request AS THE RUNNER LEFT IT.
    let job = serde_json::json!({"data":[{
        "id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open",
        "metadata":{"host":"forge","verb":"once","args":[],
                    "completion_refused": written["completion_refused"]},
        "steps":[{"id":"s-execute","spec_slug":"execute","status":"ready",
                  "metadata":{"authority_role":"platform-admin"}}]}],"total":1});
    std::fs::write(root.join("jobs.json"), job.to_string()).unwrap();
    for pass in 2..=3 {
        let (out, payload) = run(&root, &verbs, &env);
        assert_eq!(
            runs(),
            1,
            "pass {pass} re-ran a verb whose completion was refused: {out}"
        );
        assert!(payload.is_none(), "a held request is not completed: {out}");
        assert!(
            out.contains("held after a refused completion")
                && out.contains("step has unresolved blockers"),
            "the hold names itself and the server's reason: {out}"
        );
    }
}

#[test]
fn a_refused_completion_is_written_onto_its_request_with_the_servers_reason() {
    needs_jq!();
    let root = scratch("completion-refused");
    stub_sor(&root);
    let ok = root.join("ok.sh");
    write_exec(&ok, "#!/bin/sh\necho ok\n");
    let verbs = verbs_dir(
        &root,
        &[(
            "ok",
            &format!(
                r#"{{"about":"a verb that answers","hosts":["forge"],"argv":["{}"],"params":[]}}"#,
                ok.display()
            ),
        )],
    );
    let refusal = r#"{"error":"step has unresolved blockers","step_id":"s-execute","unresolved_blockers":["s-approve=pending"]}"#;
    let log = root.join("patches.jsonl");
    let env = |log: &Path| {
        vec![
            ("STUB_PUT_CODE", "409".to_string()),
            ("STUB_PUT_BODY", refusal.to_string()),
            ("STUB_PATCH_LOG", log.display().to_string()),
        ]
    };
    let refused_patch = |log: &Path, out: &str| -> serde_json::Value {
        std::fs::read_to_string(log)
            .unwrap_or_else(|e| panic!("no PATCH at all: {e}; {out}"))
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("a PATCH body is JSON"))
            .find(|v| v.get("completion_refused").is_some())
            .unwrap_or_else(|| panic!("the refused completion never reached the request: {out}"))
    };

    packet(&root, "ok", "[]");
    let (out, _) = run(&root, &verbs, &env(&log));
    assert!(
        out.contains("step has unresolved blockers"),
        "the journal line carries the server's reason, not only its status: {out}"
    );
    let r = &refused_patch(&log, &out)["completion_refused"];
    assert_eq!(r["http"], 409, "{r} / {out}");
    assert!(
        r["reason"]
            .as_str()
            .is_some_and(|s| s.contains("s-approve=pending")),
        "the server's own words ride the request: {r}"
    );
    assert_eq!(r["count"], 1, "{r}");
    assert_eq!(r["verb_ran"], true, "the verb ran before the refusal: {r}");
    let first = r["first_at"].as_str().unwrap_or_default().to_string();
    assert!(
        first.ends_with('Z') && r["last_at"] == first.as_str(),
        "a first refusal is stamped once, in UTC: {r}"
    );
    assert!(
        out.contains("failed=1"),
        "a refused completion is still a failed pass, and the unit still goes red: {out}"
    );

    // The next pass meets the same refusal. The request already carries
    // the first one, and the record accumulates rather than resets: how
    // long a request has been jammed is the number a reader wants.
    std::fs::write(
        root.join("jobs.json"),
        r#"{"data":[{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{"host":"forge","verb":"ok","args":[],"completion_refused":{"http":409,"count":4,"first_at":"2026-09-22T00:51:02Z"}},"steps":[{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{"authority_role":"platform-admin"}}]}],"total":1}"#,
    )
    .unwrap();
    let _ = std::fs::remove_file(&log);
    let (out, _) = run(&root, &verbs, &env(&log));
    let r = &refused_patch(&log, &out)["completion_refused"];
    assert_eq!(r["count"], 5, "{r} / {out}");
    assert_eq!(r["first_at"], "2026-09-22T00:51:02Z", "{r}");

    // A completion the server ACCEPTS writes no refusal.
    packet(&root, "ok", "[]");
    let _ = std::fs::remove_file(&log);
    let (out, payload) = run(
        &root,
        &verbs,
        &[("STUB_PATCH_LOG", log.display().to_string())],
    );
    assert!(payload.is_some(), "{out}");
    assert!(
        !std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("completion_refused"),
        "an accepted completion is not a refusal: {out}"
    );
}

/// The `code` the step PUT's 409 carries when another write moved the
/// row between the handler's read and its write (car 88123ae0) — the
/// constant itself, not a copy (backlog 2aa2b19e). The runner
/// recognises the refusal by this code; the `error` words are a
/// person's and free to move, so the stub answers with [`RACE_WORDS`],
/// which the server never says, and a runner still matching the words
/// would miss the race.
const STEP_CHANGED: &str = boss_jobs::step_metadata_write::STEP_CHANGED_CODE;

/// The words the stub's step-race 409 carries — deliberately not
/// `STEP_CHANGED_ERROR`, so these cases prove the match is on the code
/// and that the words recorded on a refusal are the server's own.
const RACE_WORDS: &str = "the stub's words for a lost step race — nothing was written";

fn step_changed_body() -> String {
    serde_json::json!({
        "error": RACE_WORDS,
        "code": STEP_CHANGED,
        "step_id": "s-execute",
        "hint": "nothing was written; send the same request again — the handler reads the row afresh",
    })
    .to_string()
}

/// One answering verb, a stub SoR, and a PUT log: the fixture the
/// step-race cases share.
fn race_fixture(case: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let root = scratch(case);
    stub_sor(&root);
    let ok = root.join("ok.sh");
    write_exec(&ok, "#!/bin/sh\necho ok\n");
    let verbs = verbs_dir(
        &root,
        &[(
            "ok",
            &format!(
                r#"{{"about":"a verb that answers","hosts":["forge"],"argv":["{}"],"params":[]}}"#,
                ok.display()
            ),
        )],
    );
    packet(&root, "ok", "[]");
    let puts = root.join("puts.jsonl");
    let patches = root.join("patches.jsonl");
    (root, verbs, puts, patches)
}

/// A `date` on the fixture's PATH whose `-u +%s` answers the epoch this
/// was called at, every time, so no try the runner times can cost it a
/// second; any other form goes to the real `date`, found on PATH now.
fn stop_the_clock(root: &Path) {
    let real = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .map(|d| Path::new(d).join("date"))
        .find(|p| p.is_file())
        .expect("a real date on PATH");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    write_exec(
        &root.join("bin").join("date"),
        &format!(
            "#!/bin/sh\n\
             if [ \"$#\" = 2 ] && [ \"$1\" = -u ] && [ \"$2\" = +%s ]; then echo {now}; exit 0; fi\n\
             exec {} \"$@\"\n",
            real.display()
        ),
    );
}

fn logged(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("a logged body is JSON"))
        .collect()
}

/// A COMPLETION THAT LOST THE STEP RACE IS SENT ONCE MORE (backlog
/// 2a6d0b86, review of car 88123ae0). That car makes the step PUT
/// refuse, 409 with [`STEP_CHANGED`], a write computed from a read that
/// another write has since moved, where it used to answer success and
/// erase the other write. This runner read every non-2xx completion as
/// final: it recorded `completion_refused`, and because the verb had
/// run, the request was HELD for a human — over a refusal whose own
/// words say nothing was written and the same request may be sent
/// again. So exactly that refusal earns one resend of the same body.
#[test]
fn a_completion_that_lost_the_step_race_is_sent_once_more_and_lands() {
    needs_jq!();
    let (root, verbs, puts, patches) = race_fixture("step-race-once");
    let (out, payload) = run(
        &root,
        &verbs,
        &[
            ("STUB_PUT_FIRST_CODE", "409".to_string()),
            ("STUB_PUT_FIRST_BODY", step_changed_body()),
            ("STUB_PUT_LOG", puts.display().to_string()),
            ("STUB_PATCH_LOG", patches.display().to_string()),
        ],
    );
    let sent = logged(&puts);
    assert_eq!(
        sent.len(),
        2,
        "the lost completion and exactly one resend: {out}"
    );
    for body in &sent {
        assert_eq!(
            body,
            &serde_json::json!({"status": "completed"}),
            "the resend is the same STATUS-ONLY body — it carries nothing a lost race can make stale: {out}"
        );
    }
    assert!(payload.is_some(), "{out}");
    assert!(
        !logged(&patches)
            .iter()
            .any(|p| p.get("completion_refused").is_some()),
        "a race the resend won is not a refused completion — nothing to hold: {out}"
    );
    assert!(
        out.contains("sending the same completion once more"),
        "the journal says why a second PUT went out: {out}"
    );
    assert!(
        out.contains("answered ok") && out.contains("failed=0"),
        "the request is answered and the pass is clean: {out}"
    );
}

/// ...and a completion that loses it AGAIN is refused by name, never
/// looped: the refusal is recorded on the request with the server's own
/// words, exactly as any other refused completion is.
#[test]
fn a_completion_that_loses_the_step_race_twice_is_refused_by_name() {
    needs_jq!();
    let (root, verbs, puts, patches) = race_fixture("step-race-always");
    let (out, _) = run(
        &root,
        &verbs,
        &[
            ("STUB_PUT_CODE", "409".to_string()),
            ("STUB_PUT_BODY", step_changed_body()),
            ("STUB_PUT_LOG", puts.display().to_string()),
            ("STUB_PATCH_LOG", patches.display().to_string()),
        ],
    );
    assert_eq!(logged(&puts).len(), 2, "one resend, never a loop: {out}");
    assert!(
        out.contains("lost the step race twice"),
        "the journal names the second loss: {out}"
    );
    let refused = logged(&patches)
        .into_iter()
        .find(|p| p.get("completion_refused").is_some())
        .unwrap_or_else(|| panic!("the second loss is a refused completion: {out}"));
    assert!(
        refused["completion_refused"]["reason"]
            .as_str()
            .is_some_and(|s| s.contains(RACE_WORDS)),
        "the refusal rides the request in the server's words: {refused}"
    );
    assert!(out.contains("failed=1"), "{out}");
}

/// Any OTHER refused completion is still sent exactly once: only the
/// refusal that says nothing was written earns a resend.
#[test]
fn a_completion_refused_for_another_reason_is_not_resent() {
    needs_jq!();
    let (root, verbs, puts, patches) = race_fixture("put-409-other");
    let (out, _) = run(
        &root,
        &verbs,
        &[
            ("STUB_PUT_CODE", "409".to_string()),
            (
                "STUB_PUT_BODY",
                r#"{"error":"step has unresolved blockers"}"#.to_string(),
            ),
            ("STUB_PUT_LOG", puts.display().to_string()),
            ("STUB_PATCH_LOG", patches.display().to_string()),
        ],
    );
    assert_eq!(logged(&puts).len(), 1, "never resent: {out}");
    assert!(!out.contains("once more"), "{out}");
}

/// THE RACE IS NAMED BY ITS CODE, NOT ITS WORDS (backlog 2aa2b19e). A
/// 409 carrying the server's own words for the race but no `code` is
/// some other refusal as far as the runner can tell, so it is not
/// resent: the words were five byte-equal copies held together by
/// nothing, and a runner that matched them broke the day a person
/// reworded them (backlog 428332da).
#[test]
fn a_409_that_only_says_the_words_is_not_the_step_race() {
    needs_jq!();
    let (root, verbs, puts, patches) = race_fixture("put-409-words-only");
    let words_only = serde_json::json!({
        "error": boss_jobs::step_metadata_write::STEP_CHANGED_ERROR,
        "step_id": "s-execute",
    })
    .to_string();
    let (out, _) = run(
        &root,
        &verbs,
        &[
            ("STUB_PUT_CODE", "409".to_string()),
            ("STUB_PUT_BODY", words_only),
            ("STUB_PUT_LOG", puts.display().to_string()),
            ("STUB_PATCH_LOG", patches.display().to_string()),
        ],
    );
    assert_eq!(
        logged(&puts).len(),
        1,
        "matched by code, never by words: {out}"
    );
    assert!(!out.contains("once more"), "{out}");
}

/// FOR ONE RELEASE, THE OLD WORDS ARE THE RACE TOO (the review of car
/// 24eb9471). A jobs API from before the code answers the race with its
/// old words and no `code`; a runner rolled out ahead of it must still
/// resend. Delete with `STEP_CHANGED_ERROR_BEFORE_THE_CODE`.
#[test]
fn a_409_in_the_old_words_is_the_step_race_for_one_release() {
    needs_jq!();
    let (root, verbs, puts, patches) = race_fixture("put-409-old-words");
    let old = serde_json::json!({
        "error": boss_jobs::step_metadata_write::STEP_CHANGED_ERROR_BEFORE_THE_CODE,
        "step_id": "s-execute",
    })
    .to_string();
    let (out, _) = run(
        &root,
        &verbs,
        &[
            ("STUB_PUT_FIRST_CODE", "409".to_string()),
            ("STUB_PUT_FIRST_BODY", old),
            ("STUB_PUT_LOG", puts.display().to_string()),
            ("STUB_PATCH_LOG", patches.display().to_string()),
        ],
    );
    assert_eq!(logged(&puts).len(), 2, "resent once: {out}");
    assert!(
        out.contains("answered ok") && out.contains("failed=0"),
        "{out}"
    );
}

/// A STEP WRITE THE SYSTEM OF RECORD DID NOT ANSWER IS TRIED AGAIN (the
/// review of car 24eb9471). A completion is two writes, so a roll
/// between them stranded the keys on an open step; each write now rides
/// out a 5xx or no answer on OPS_WRITE_BACKOFF, as the gate runner's
/// report does.
#[test]
fn a_step_write_met_by_a_roll_is_tried_again_and_lands() {
    needs_jq!();
    let (root, verbs, puts, patches) = race_fixture("merge-503-once");
    let merges = root.join("merges.jsonl");
    let (out, _) = run(
        &root,
        &verbs,
        &[
            ("OPS_WRITE_BACKOFF", "0 0".to_string()),
            ("STUB_MERGE_FIRST_CODE", "503".to_string()),
            ("STUB_MERGE_LOG", merges.display().to_string()),
            ("STUB_PUT_LOG", puts.display().to_string()),
            ("STUB_PATCH_LOG", patches.display().to_string()),
        ],
    );
    assert_eq!(
        logged(&merges).len(),
        2,
        "the merge again after the 503: {out}"
    );
    assert_eq!(logged(&puts).len(), 1, "then the status: {out}");
    assert!(out.contains("may be rolling"), "{out}");
    assert!(
        out.contains("answered ok") && out.contains("failed=0"),
        "{out}"
    );
}

/// A verb that counts its runs, a stub SoR, and the logs: the fixture
/// the never-run-twice cases share (review of car 59a6ade9).
fn counted_fixture(case: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = scratch(case);
    stub_sor(&root);
    let ran = root.join("ran.count");
    let verb = root.join("verb.sh");
    write_exec(
        &verb,
        &format!("#!/bin/sh\necho x >> \"{}\"\necho did it\n", ran.display()),
    );
    let verbs = verbs_dir(
        &root,
        &[(
            "once",
            &format!(
                r#"{{"about":"a verb that must not repeat","hosts":["forge"],"argv":["{}"],"params":[]}}"#,
                verb.display()
            ),
        )],
    );
    packet(&root, "once", "[]");
    (root, verbs, ran)
}

fn runs(ran: &Path) -> usize {
    std::fs::read_to_string(ran)
        .unwrap_or_default()
        .lines()
        .count()
}

/// The request as the runner left it when the keys landed and the
/// status did not: execute still READY, carrying this host's answer —
/// and, when given, the `completion_refused` the runner recorded.
fn left_answered(root: &Path, completion_refused: Option<&serde_json::Value>) {
    let mut md = serde_json::json!({"host": "forge", "verb": "once", "args": []});
    if let Some(r) = completion_refused {
        md["completion_refused"] = r.clone();
    }
    let job = serde_json::json!({"data": [{
        "id": "aaaaaaaa-0000-4000-8000-000000000000", "status": "open", "metadata": md,
        "steps": [{"id": "s-execute", "spec_slug": "execute", "status": "ready",
                   "metadata": {"authority_role": "platform-admin", "disposition": "answered",
                                "output": "did it\n", "exit_code": "0",
                                "runner_host": "forge"}}]}], "total": 1});
    std::fs::write(root.join("jobs.json"), job.to_string()).unwrap();
}

/// A SERVER THAT ANSWERS 5xx HAS ANSWERED (the re-review of car
/// 59a6ade9). A status PUT the server keeps answering 500 — a failed
/// blocker check, a failed protocol read, a repository error — is a
/// refused completion, recorded with `verb_ran`, as main recorded it: the
/// held check then stops the next pass from running the verb again. Read
/// as an outage it recorded nothing, execute stayed ready carrying this
/// host's answer, and every later pass RAN THE VERB AGAIN — rollback-to,
/// delete-orphan-object, retire-* among them: post-mortem 3c3b202c
/// again. And once a person clears the refusal, the answer already on
/// the step is finished with the status alone, never run again.
#[test]
fn a_status_the_server_keeps_refusing_with_a_5xx_never_runs_the_verb_again() {
    needs_jq!();
    let (root, verbs, ran) = counted_fixture("status-500-always");
    let puts = root.join("puts.jsonl");
    let patches = root.join("patches.jsonl");
    let merges = root.join("merges.jsonl");
    let env = |code: &str| {
        vec![
            ("OPS_WRITE_BACKOFF", "0 0".to_string()),
            ("STUB_PUT_CODE", code.to_string()),
            (
                "STUB_PUT_BODY",
                r#"{"error":"reading the packet's protocol failed"}"#.to_string(),
            ),
            ("STUB_PUT_LOG", puts.display().to_string()),
            ("STUB_PATCH_LOG", patches.display().to_string()),
            ("STUB_MERGE_LOG", merges.display().to_string()),
        ]
    };

    // Pass 1: the verb runs, its keys land, its status meets 500 on every try.
    let (out, _) = run(&root, &verbs, &env("500"));
    assert_eq!(runs(&ran), 1, "{out}");
    assert_eq!(
        logged(&puts).len(),
        3,
        "the first try and one per delay: {out}"
    );
    let refused = logged(&patches)
        .into_iter()
        .find(|p| p.get("completion_refused").is_some())
        .unwrap_or_else(|| panic!("a 5xx is a refused completion, recorded: {out}"));
    assert_eq!(refused["completion_refused"]["http"], 500, "{refused}");
    assert_eq!(refused["completion_refused"]["verb_ran"], true, "{refused}");
    assert!(out.contains("failed=1"), "{out}");

    // Pass 2 sees the request as pass 1 left it: HELD, the verb not run.
    left_answered(&root, Some(&refused["completion_refused"]));
    let _ = std::fs::remove_file(&puts);
    let (out, _) = run(&root, &verbs, &env("500"));
    assert_eq!(
        runs(&ran),
        1,
        "a held request never re-runs its verb: {out}"
    );
    assert!(out.contains("held after a refused completion"), "{out}");

    // Pass 3, after a person clears the refusal: the answer on the step
    // is finished with the status alone — and still not run again.
    left_answered(&root, None);
    let _ = std::fs::remove_file(&puts);
    let _ = std::fs::remove_file(&merges);
    let (out, _) = run(&root, &verbs, &env("200"));
    assert_eq!(
        runs(&ran),
        1,
        "this host's answer is finished, never re-run: {out}"
    );
    assert_eq!(
        logged(&puts),
        vec![serde_json::json!({"status": "completed"})],
        "{out}"
    );
    assert!(logged(&merges).is_empty(), "nothing to merge: {out}");
    assert!(
        out.contains("answered=1") && out.contains("failed=0"),
        "{out}"
    );
}

/// NO ANSWER AT ALL IS AN OUTAGE, and nothing can be recorded through a
/// system of record that is not answering — so the pass fails,
/// unrecorded. The next pass finds this host's answer on a ready execute
/// (backlog 1434e6e3) and finishes it with the status alone: the verb
/// never runs twice. And a deterministic 5xx on THAT finish is recorded,
/// never looped silently.
#[test]
fn a_status_that_is_never_answered_is_finished_by_the_next_pass_not_re_run() {
    needs_jq!();
    let (root, verbs, ran) = counted_fixture("status-000-always");
    let puts = root.join("puts.jsonl");
    let patches = root.join("patches.jsonl");
    let env = |code: &str| {
        vec![
            ("OPS_WRITE_BACKOFF", "0 0".to_string()),
            ("STUB_PUT_CODE", code.to_string()),
            (
                "STUB_PUT_BODY",
                r#"{"error":"a repository error"}"#.to_string(),
            ),
            ("STUB_PUT_LOG", puts.display().to_string()),
            ("STUB_PATCH_LOG", patches.display().to_string()),
        ]
    };

    let (out, _) = run(&root, &verbs, &env("000"));
    assert_eq!(runs(&ran), 1, "{out}");
    assert_eq!(
        logged(&puts).len(),
        3,
        "the first try and one per delay: {out}"
    );
    assert!(
        !logged(&patches)
            .iter()
            .any(|p| p.get("completion_refused").is_some()),
        "no answer, so nothing could be recorded: {out}"
    );
    assert!(out.contains("failed=1"), "{out}");

    // Pass 2: this host's answer is on a ready execute — finished, not re-run.
    left_answered(&root, None);
    let _ = std::fs::remove_file(&puts);
    let (out, _) = run(&root, &verbs, &env("200"));
    assert_eq!(runs(&ran), 1, "the verb never runs twice: {out}");
    assert_eq!(
        logged(&puts),
        vec![serde_json::json!({"status": "completed"})],
        "{out}"
    );
    assert!(
        out.contains("answered=1") && out.contains("failed=0"),
        "{out}"
    );

    // The same finish meeting a deterministic 5xx records it.
    let _ = std::fs::remove_file(&puts);
    let _ = std::fs::remove_file(&patches);
    let (out, _) = run(&root, &verbs, &env("500"));
    assert_eq!(runs(&ran), 1, "{out}");
    let refused = logged(&patches)
        .into_iter()
        .find(|p| p.get("completion_refused").is_some())
        .unwrap_or_else(|| panic!("a 5xx on the finish is recorded, not looped: {out}"));
    assert_eq!(refused["completion_refused"]["verb_ran"], true, "{refused}");
    assert!(out.contains("failed=1"), "{out}");
}

/// A merge the server keeps answering 5xx is a refused completion too:
/// no status follows the unlanded keys, and the refusal is recorded so
/// the held check stops a re-run.
#[test]
fn a_merge_the_server_keeps_refusing_with_a_5xx_is_recorded_and_held() {
    needs_jq!();
    let (root, verbs, puts, patches) = race_fixture("merge-503-always");
    let merges = root.join("merges.jsonl");
    let (out, _) = run(
        &root,
        &verbs,
        &[
            ("OPS_WRITE_BACKOFF", "0 0".to_string()),
            ("STUB_MERGE_CODE", "503".to_string()),
            ("STUB_MERGE_LOG", merges.display().to_string()),
            ("STUB_PUT_LOG", puts.display().to_string()),
            ("STUB_PATCH_LOG", patches.display().to_string()),
        ],
    );
    assert_eq!(
        logged(&merges).len(),
        3,
        "the first try and one per delay: {out}"
    );
    assert!(
        logged(&puts).is_empty(),
        "no status after unlanded keys: {out}"
    );
    let refused = logged(&patches)
        .into_iter()
        .find(|p| p.get("completion_refused").is_some())
        .unwrap_or_else(|| panic!("a 5xx merge is a refused completion: {out}"));
    assert_eq!(refused["completion_refused"]["http"], 503, "{refused}");
    assert_eq!(refused["completion_refused"]["verb_ran"], true, "{refused}");
    assert!(out.contains("failed=1"), "{out}");
}

/// ONE PASS HAS ONE RETRY BUDGET (the re-review of car 59a6ade9). The
/// backoff is per write, so a dark system of record met by a pass with
/// many requests would stretch that pass past APPROVAL_TTL_S and let
/// approvals it has not reached yet expire behind it. The retries of a
/// whole pass stop when OPS_WRITE_RETRY_BUDGET seconds are spent, and
/// the default sits below APPROVAL_TTL_S — a value at or above it is cut
/// to half the window.
///
/// The runner charges a failed try's own time as `date -u +%s` before
/// and after it, in whole seconds, so against a real clock a first try
/// that straddled a second boundary cost 1s, the budget of 1 was spent
/// before the first retry, and the case sent 1 PUT, not 2: red on
/// gates a742109c and f9c83e8d, and 11 of 200 runs under 48 spinners
/// on the dev pod (backlog 2f97de89). A tick decided it, not the budget. So
/// the clock is stopped for this case: `date -u +%s` answers one fixed
/// epoch and every other `date` goes to the real one, which leaves the
/// sleeps as the only spend — the logic the case pins.
#[test]
fn a_pass_stops_retrying_when_its_budget_is_spent_and_the_budget_sits_below_the_approval_window() {
    needs_jq!();
    let (root, verbs, puts, patches) = race_fixture("retry-budget");
    stop_the_clock(&root);
    let (out, _) = run(
        &root,
        &verbs,
        &[
            ("OPS_WRITE_BACKOFF", "1 1 1 1".to_string()),
            ("OPS_WRITE_RETRY_BUDGET", "1".to_string()),
            ("STUB_PUT_CODE", "500".to_string()),
            ("STUB_PUT_LOG", puts.display().to_string()),
            ("STUB_PATCH_LOG", patches.display().to_string()),
        ],
    );
    assert_eq!(
        logged(&puts).len(),
        2,
        "one retry fits a one-second budget, the next does not: {out}"
    );
    assert!(out.contains("retry budget"), "{out}");

    let src = std::fs::read_to_string(repo_root().join("infra/ops/ops-runner.sh")).unwrap();
    let number = |prefix: &str| -> u64 {
        src.lines()
            .find_map(|l| l.strip_prefix(prefix))
            .and_then(|rest| {
                rest.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .ok()
            })
            .unwrap_or_else(|| panic!("ops-runner.sh has no `{prefix}<n>` line"))
    };
    let budget = number("OPS_WRITE_RETRY_BUDGET=\"${OPS_WRITE_RETRY_BUDGET:-");
    let ttl = number("APPROVAL_TTL_S=");
    assert!(
        budget < ttl,
        "the default retry budget ({budget}s) must sit below APPROVAL_TTL_S ({ttl}s)"
    );
}

/// A COMPLETION SENDS ONLY WHAT IT CHANGES (backlog 2aa2b19e, the
/// direction of e39a9d2a). The completion was one PUT of `{status,
/// metadata}`, the metadata being every key the runner READ at poll time
/// with its own laid over them. The step PUT replaces metadata
/// wholesale, so that body re-sent keys the runner never meant to
/// write — stale by the time it landed if anything wrote the step in
/// between — and it leaned on the server's omitted-keys refusal and the
/// row-version compare to be safe. The gate runner moved off that shape
/// first: the keys go through the step merge door, which merges against
/// the row as it stands, and the PUT that follows carries the status
/// alone, so a resend after a lost race has nothing to be stale.
#[test]
fn a_completion_sends_its_keys_through_the_merge_door_and_the_status_alone() {
    needs_jq!();
    let (root, verbs, puts, patches) = race_fixture("completion-shape");
    std::fs::write(
        root.join("jobs.json"),
        serde_json::json!({"data": [{
            "id": "aaaaaaaa-0000-4000-8000-000000000000", "status": "open",
            "metadata": {"host": "forge", "verb": "ok", "args": []},
            "steps": [{"id": "s-execute", "spec_slug": "execute", "status": "ready",
                       "metadata": {"authority_role": "platform-admin", "station": "ops",
                                    "procedure": "the protocol's words"}}]}], "total": 1})
        .to_string(),
    )
    .unwrap();
    let merges = root.join("merges.jsonl");
    let order = root.join("order.log");
    let (out, _) = run(
        &root,
        &verbs,
        &[
            ("STUB_PUT_LOG", puts.display().to_string()),
            ("STUB_PATCH_LOG", patches.display().to_string()),
            ("STUB_MERGE_LOG", merges.display().to_string()),
            ("STUB_WRITE_ORDER", order.display().to_string()),
        ],
    );
    let merged = logged(&merges);
    assert_eq!(merged.len(), 1, "one merge: {out}");
    let md = &merged[0];
    for read_only in ["authority_role", "station", "procedure"] {
        assert!(
            md.get(read_only).is_none(),
            "{read_only} was READ, not written — a completion that re-sends it can only make it stale: {md}"
        );
    }
    assert_eq!(md["disposition"], "answered", "{md}");
    assert_eq!(md["exit_code"], "0", "{md}");
    assert_eq!(md["runner_host"], "forge", "{md}");
    // Every key an answer may carry and THIS one does not is sent as
    // null, which the merge door reads as a delete — so an answer never
    // sits beside a refusal reason, or an approval, left by another pass
    // (the re-review of car 59a6ade9).
    for cleared in [
        "reason",
        "approved_plan_sha256",
        "approval_signed_at",
        "claimed_as",
        // This fixture's verb declares no effect (backlog fdbb447e).
        "effect",
        "effect_unproven",
        "effect_unread",
    ] {
        assert_eq!(
            md.get(cleared),
            Some(&serde_json::Value::Null),
            "{cleared} is cleared, not left: {md}"
        );
    }
    assert!(
        md["output"].as_str().is_some_and(|s| s.contains("ok")),
        "{md}"
    );
    assert_eq!(
        logged(&puts),
        vec![serde_json::json!({"status": "completed"})],
        "the PUT carries the status and nothing else: {out}"
    );
    let writes: Vec<String> = std::fs::read_to_string(&order)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("/steps/"))
        .map(str::to_string)
        .collect();
    assert_eq!(
        writes,
        vec![
            "PATCH http://sor.invalid/api/jobs/aaaaaaaa-0000-4000-8000-000000000000/steps/s-execute/metadata".to_string(),
            "PUT http://sor.invalid/api/jobs/aaaaaaaa-0000-4000-8000-000000000000/steps/s-execute".to_string(),
        ],
        "the keys land BEFORE the status: a step's required-at-done fields are judged when it flips: {out}"
    );
    assert!(out.contains("failed=0"), "{out}");
}

/// A MERGE THE SERVER REFUSES IS A REFUSED COMPLETION: no status goes
/// after it (the fields a completion is judged on are not on the row),
/// and the refusal is written onto the request in the server's words,
/// exactly as a refused PUT is (post-mortem 3c3b202c).
#[test]
fn a_refused_merge_sends_no_status_and_is_recorded_on_the_request() {
    needs_jq!();
    let (root, verbs, puts, patches) = race_fixture("merge-refused");
    let (out, payload) = run(
        &root,
        &verbs,
        &[
            ("STUB_MERGE_CODE", "409".to_string()),
            (
                "STUB_MERGE_BODY",
                r#"{"error":"metadata patch changes a key the protocol owns"}"#.to_string(),
            ),
            ("STUB_PUT_LOG", puts.display().to_string()),
            ("STUB_PATCH_LOG", patches.display().to_string()),
        ],
    );
    assert!(payload.is_some(), "the merge was attempted: {out}");
    assert!(
        logged(&puts).is_empty(),
        "no status after a refused merge: {out}"
    );
    let refused = logged(&patches)
        .into_iter()
        .find(|p| p.get("completion_refused").is_some())
        .unwrap_or_else(|| panic!("the refused merge is a refused completion: {out}"));
    assert_eq!(refused["completion_refused"]["http"], 409, "{refused}");
    assert!(
        refused["completion_refused"]["reason"]
            .as_str()
            .is_some_and(|s| s.contains("a key the protocol owns")),
        "{refused}"
    );
    assert!(
        out.contains("a key the protocol owns") && out.contains("failed=1"),
        "{out}"
    );
}

/// A QUEUE WHOSE DEPTH NOBODY READS (backlog 1ffb3305). This runner
/// walks up to 100 open requests SERIALLY in one oneshot with no
/// per-verb fairness, so a latency-sensitive verb queues behind
/// whatever is ahead of it in the same run — a converge, the verb that
/// deploys a fix, waits behind however many run-car-probes are in
/// front of it. Measured 2026-09-19: run-car-probe was 171 of the last
/// 300 ops-requests against 42 converges, and its cadence went daily to
/// hourly the same day. Today's volumes are comfortable; the serial
/// walk is now the only real bound on raising any probe cadence
/// further, and nothing measured it. A queue nobody reads is one that
/// gets discovered at its worst moment, by a converge that did not
/// deploy when it should have.
///
/// So the runner reads its own queue before it walks it, and the
/// reading lands in two places on purpose: the journal line carries
/// the gauge for EVERY run, depth zero included — a gauge that appears
/// only when it is non-zero cannot be told apart from a runner that
/// stopped — and each answered request carries what IT waited, so the
/// series is a system-of-record query rather than an ssh. Per-verb
/// fairness or a priority lane is the larger change and waits for this
/// reading to say it is needed.
#[test]
fn the_runner_reads_its_own_queue_depth_and_oldest_wait() {
    needs_jq!();
    let root = scratch("queue-reading");
    stub_sor(&root);
    let ok = root.join("ok.sh");
    write_exec(&ok, "#!/bin/sh\necho ok\n");
    let verbs = verbs_dir(
        &root,
        &[(
            "ok",
            &format!(
                r#"{{"about":"a verb that answers","hosts":["forge"],"argv":["{}"],"params":[]}}"#,
                ok.display()
            ),
        )],
    );

    // An empty queue is a reading too, and the one the runner takes
    // most often.
    std::fs::write(root.join("jobs.json"), r#"{"data":[],"total":0}"#).unwrap();
    let (out, _) = run(&root, &verbs, &[]);
    assert_eq!(
        queue_line(&out),
        "ops-runner: queue host=forge depth=0 oldest_wait_s=-",
        "an empty queue still reports its depth: {out}"
    );

    // Three open requests for this host, the oldest filed 600s ago.
    queue(&root, "ok", &[Some(120), Some(600), Some(30)]);
    let (out, _) = run(&root, &verbs, &[]);
    let line = queue_line(&out);
    assert!(line.contains("depth=3"), "{line}");
    let oldest: i64 = field(&line, "oldest_wait_s")
        .parse()
        .unwrap_or_else(|_| panic!("oldest_wait_s is a number: {line}"));
    assert!(
        (600..660).contains(&oldest),
        "the oldest wait is the oldest packet's age, not the newest's: {line}"
    );

    // A packet nobody stamped has NO age, and the reading says so
    // rather than answering zero: `date -d ''` answers midnight, which
    // would report a fresh packet as a decades-old wait (the probe-time
    // trap, crates/core/boss-jobs/src/probe.rs).
    queue(&root, "ok", &[None]);
    let (out, _) = run(&root, &verbs, &[]);
    assert_eq!(
        queue_line(&out),
        "ops-runner: queue host=forge depth=1 oldest_wait_s=-",
        "an unstamped packet has no age: {out}"
    );

    // And the answered request carries what IT waited, beside the exit
    // that already rides there — so the depth series is a jobs-API
    // query, not a journal nobody opens.
    queue(&root, "ok", &[Some(300), Some(45)]);
    let (out, _) = run(&root, &verbs, &[]);
    let patch: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("patch.json"))
            .unwrap_or_else(|e| panic!("no PATCH on the request's metadata: {e}; {out}")),
    )
    .expect("the PATCH body is JSON");
    assert_eq!(patch["queue_depth"], 2, "{patch} / {out}");
    let waited = patch["queued_s"]
        .as_i64()
        .unwrap_or_else(|| panic!("the request records its own wait: {patch} / {out}"));
    assert!(
        (45..105).contains(&waited),
        "the LAST packet walked waited its own 45s, not the queue's oldest: {patch}"
    );
}

/// A LIMIT IS NOT A FILTER (backlog 2cfb4562). The reading above was
/// taken from `?kind=ops-request&status=open&limit=100` — EVERY host's
/// open requests, one page of them, with the `total` the API answers
/// beside the rows thrown away. At a depth above 100 the walk silently
/// ignored the tail and the gauge printed the page as though it were
/// the whole queue: a queue stuck at 400 reads 100 forever, and any
/// threshold set above 100 could never be crossed. The same shape was
/// found the same day in a recorded probe (limit=300 against a live
/// total of 345, its one qualifying row in the unread tail).
///
/// So the query is narrowed to THIS host by the server, the rows are
/// compared against the `total` it answers, and a reading that could
/// not see the whole queue says so in a shape no reader can take for
/// an exact number: `depth=` only when the count is the server's own
/// for this host, `depth>=` when it is a lower bound, and the oldest
/// wait likewise — the list is newest first, so the tail it did not
/// read is exactly where the oldest packets are.
#[test]
fn a_queue_the_runner_could_not_see_all_of_is_not_reported_as_exact() {
    needs_jq!();
    let root = scratch("queue-truncated");
    stub_sor(&root);
    let ok = root.join("ok.sh");
    write_exec(&ok, "#!/bin/sh\necho ok\n");
    let verbs = verbs_dir(
        &root,
        &[(
            "ok",
            &format!(
                r#"{{"about":"a verb that answers","hosts":["forge"],"argv":["{}"],"params":[]}}"#,
                ok.display()
            ),
        )],
    );

    // The server is asked for THIS host's queue, not every host's
    // first page: the host rides the containment filter, url-encoded.
    queue(&root, "ok", &[Some(30)]);
    let (out, _) = run(&root, &verbs, &[]);
    let url = std::fs::read_to_string(root.join("get.url"))
        .unwrap_or_else(|e| panic!("the runner made no list read: {e}; {out}"));
    assert!(
        url.contains("metadata=%7B%22host%22%3A%22forge%22%7D"),
        "the list read is narrowed to this host by the server: {url}"
    );
    assert!(
        !url.split(['?', '&']).any(|kv| kv.trim() == "limit=100"),
        "the page is not the old 100 cap: {url}"
    );

    // Two rows on the page, five open: the depth is the server's count
    // and the walk knows it saw only part of it.
    queue_of(&root, "ok", &[Some(120), Some(30)], Some(5));
    let (out, _) = run(&root, &verbs, &[]);
    let line = queue_line(&out);
    assert_eq!(
        field(&line, "depth"),
        "5",
        "the depth is the server's total: {line}"
    );
    assert!(
        line.contains("oldest_wait_s>=") && line.contains("truncated"),
        "the oldest wait of a partial read is a lower bound, and the line says why: {line}"
    );
    assert!(
        !line
            .split_whitespace()
            .any(|w| w.starts_with("oldest_wait_s=")),
        "a lower bound is never printed in the exact shape: {line}"
    );
    let patch = read_patch(&root, &out);
    assert_eq!(
        patch["queue_depth"], 5,
        "the request carries the whole queue's depth: {patch}"
    );

    // No `total` at all: the runner cannot know how much it did not
    // see, so it REFUSES the exact reading rather than printing the
    // page as the queue — and the request says "at least", under a key
    // no reader of `queue_depth` can mistake for the count.
    queue_of(&root, "ok", &[Some(120), Some(30)], None);
    let (out, _) = run(&root, &verbs, &[]);
    let line = queue_line(&out);
    assert!(
        line.contains("depth>=2") && line.contains("truncated"),
        "an unverifiable count is a lower bound, named: {line}"
    );
    assert!(
        !line.split_whitespace().any(|w| w.starts_with("depth=")),
        "no exact depth without a total to check it against: {line}"
    );
    let patch = read_patch(&root, &out);
    assert!(
        patch.get("queue_depth").is_none(),
        "a lower bound is not written as the depth: {patch}"
    );
    assert_eq!(patch["queue_depth_at_least"], 2, "{patch}");

    // A server that ignored the host filter answers every host's
    // `total` — the wrong-target shape (CLAUDE.md §Doors: it answers
    // instead of erroring). A row for another host on the page is the
    // evidence, and that total is not this host's depth.
    std::fs::write(
        root.join("jobs.json"),
        r#"{"data":[{"id":"bbbbbbbb-0000-4000-8000-000000000000","status":"open","metadata":{"host":"boss-gcp","verb":"ok","args":[]},"steps":[]}],"total":1}"#,
    )
    .unwrap();
    let (out, _) = run(&root, &verbs, &[]);
    let line = queue_line(&out);
    assert!(
        line.contains("depth>=0") && line.contains("truncated"),
        "another host's total is not this host's depth: {line}"
    );
}

/// The request-level PATCH the last run wrote, or a panic naming what
/// the run printed instead.
fn read_patch(root: &Path, out: &str) -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(root.join("patch.json"))
            .unwrap_or_else(|e| panic!("no PATCH on the request's metadata: {e}; {out}")),
    )
    .expect("the PATCH body is JSON")
}

/// The runner's one-line queue reading, or a panic naming what it
/// printed instead.
fn queue_line(out: &str) -> String {
    out.lines()
        .find(|l| l.starts_with("ops-runner: queue "))
        .unwrap_or_else(|| panic!("no queue reading in the run's output:\n{out}"))
        .to_string()
}

/// `key=value` off a space-separated reading line.
fn field(line: &str, key: &str) -> String {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key} in {line}"))
        .to_string()
}

/// A queue of open ops-requests for the forge, one per entry, each
/// carrying the `opened_at` a live request carries (`Some(age)`
/// seconds ago) or none at all.
fn queue(root: &Path, verb: &str, ages_s: &[Option<u64>]) {
    queue_of(root, verb, ages_s, Some(ages_s.len()));
}

/// [`queue`], with the `total` the server answers beside the rows set
/// by hand: `Some(n)` larger than the rows is a page that did not hold
/// the whole queue, and `None` is a response that carries no `total`.
fn queue_of(root: &Path, verb: &str, ages_s: &[Option<u64>], total: Option<usize>) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let rows: Vec<String> = ages_s
        .iter()
        .enumerate()
        .map(|(i, age)| {
            let opened = match age {
                Some(a) => format!(r#","opened_at":"{}""#, iso_at(now - a)),
                None => String::new(),
            };
            format!(
                r#"{{"id":"aaaaaaaa-0000-4000-8000-00000000000{i}","status":"open","metadata":{{"host":"forge","verb":"{verb}","args":[]{opened}}},"steps":[{{"id":"s-{i}","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}"#
            )
        })
        .collect();
    std::fs::write(
        root.join("jobs.json"),
        match total {
            Some(t) => format!(r#"{{"data":[{}],"total":{t}}}"#, rows.join(",")),
            None => format!(r#"{{"data":[{}]}}"#, rows.join(",")),
        },
    )
    .unwrap();
}

/// The timestamp shape a live ops-request carries in `metadata.opened_at`
/// (read off request e8248c26, 2026-09-20): RFC 3339, nanoseconds, an
/// explicit `+00:00` offset.
fn iso_at(epoch: u64) -> String {
    let out = Command::new("date")
        .args([
            "-u",
            "-d",
            &format!("@{epoch}"),
            "+%Y-%m-%dT%H:%M:%S.000000000+00:00",
        ])
        .output()
        .expect("date runs");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A verb that declares `requires_approval` is REFUSED unless the runner
/// verified an approval — and the refusal says which verb, and why.
///
/// This was the safety half of design 17835005 (passkey-approved,
/// machine-executed destructive operations), and it landed BEFORE the
/// thing that issues approvals, deliberately: the guard matters more than
/// the approval, so the first such verb was inert on arrival. The
/// approval channel landed later (backlog fd7090cc; its cases are
/// `ops_runner_approval_sh.rs`), and this case keeps the floor under it:
/// a request with no approve step at all — nothing any approval could
/// ride on — is still refused, naming the verb.
///
/// FAIL CLOSED IS THE WHOLE POINT. A runner that cannot check an
/// approval must refuse, never assume; `commission-a-disk` partitions a
/// block device, and the failure mode of assuming is a wiped host.
#[test]
fn a_verb_requiring_approval_is_refused_without_a_verified_approval() {
    needs_jq!();
    let root = scratch("requires-approval");
    stub_sor(&root);
    let verbs = verbs_dir(
        &root,
        &[(
            "needs-a-human",
            r#"{"about":"MUTATING — test fixture.","hosts":["forge"],
                "requires_approval":true,
                "argv":["true"],"params":[]}"#,
        )],
    );
    packet(&root, "needs-a-human", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md}");
    let reason = md["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("no reason on the step: {md}"));
    assert!(
        reason.contains("needs-a-human") && reason.contains("approval"),
        "the refusal names the verb and what it wants: {reason}"
    );
    assert!(
        md["exit_code"].is_null(),
        "a refusal ran nothing — and this one in particular must not: {md}"
    );
}

/// The refusal is about the DECLARATION, not about the verb being
/// unusual: an ordinary verb in the same allowlist still runs. Without
/// this control the test above passes against a runner that refuses
/// everything.
#[test]
fn a_verb_that_requires_no_approval_still_runs_beside_one_that_does() {
    needs_jq!();
    let root = scratch("requires-approval-control");
    stub_sor(&root);
    let verbs = verbs_dir(
        &root,
        &[
            (
                "needs-a-human",
                r#"{"about":"MUTATING — test fixture.","hosts":["forge"],
                    "requires_approval":true,"argv":["true"],"params":[]}"#,
            ),
            (
                "ordinary",
                r#"{"about":"a read.","hosts":["forge"],"argv":["true"],"params":[]}"#,
            ),
        ],
    );
    packet(&root, "ordinary", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md}");
    assert_eq!(md["exit_code"], "0", "{md}");
}

/// The shipped verb declares it, and the shipped script refuses the
/// shapes that would aim it at the wrong disk — or at a plan nobody
/// approved.
///
/// The declaration is a one-word difference between a verb that runs
/// only under a passkey and a verb that partitions a block device on
/// request, so it is pinned rather than trusted to review. Since backlog
/// b2d5b546 (2026-09-26) the script takes the approved plan's sha256 as
/// its last argument, the way merge-tenant-main and reap-terminated-pods
/// do: without one it is a refusal before any device is looked at.
#[test]
fn the_disk_verb_requires_approval_and_refuses_an_unstable_target() {
    let root = repo_root();
    let spec: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("infra/ops/verbs/commission-a-disk.json"))
            .expect("the verb file"),
    )
    .expect("the verb file is JSON");
    assert_eq!(
        spec["requires_approval"], true,
        "commission-a-disk writes a partition table; it runs only under a verified passkey \
         approval"
    );
    assert_eq!(
        spec["argv"],
        serde_json::json!(["infra/forge/commission-a-disk.sh", "{1}", "{2}", "{3}"]),
        "the runner appends the SIGNED plan's hash as the last arg, and the script must \
         receive it"
    );
    // The allowlist holds the mount path to the script's own shape, so a
    // request for any other path is refused before a plan is rendered.
    assert_eq!(
        spec["params"][1]["pattern"], "^/srv/[A-Za-z0-9][A-Za-z0-9._-]{0,40}$",
        "one new directory under /srv, nothing else (review of car 0556935a)"
    );
    assert_eq!(
        spec["params"][0]["pattern"], "^/dev/disk/by-id/[A-Za-z0-9._-]{4,120}$",
        "a by-id name, never a kernel name"
    );
    assert!(
        spec["about"]
            .as_str()
            .unwrap_or_default()
            .contains("MUTATING"),
        "the bounded-verbs lint derives its roster from that word"
    );

    // The script's refusals, exercised. A success path cannot be tested
    // here — it would partition the gate runner — so the failure paths
    // are the whole test, which is the right way round for a verb whose
    // only interesting property is what it declines to do.
    let script = root.join("infra/forge/commission-a-disk.sh");
    let hash = "0123456789abcdef".repeat(4);
    let by_id = "/dev/disk/by-id/nvme-NO-SUCH-DEVICE-0000";
    let cases: [(&[&str], &str); 20] = [
        (&["/dev/nvme0n1", "/srv/data", &hash], "stable identity"),
        (&["nvme0n1", "/srv/data", &hash], "stable identity"),
        (&[by_id, "/srv/data", &hash], "no such device"),
        // A by-id name for a PARTITION is refused by its name, before it
        // is resolved (the review of car 0556935a, finding 1: a
        // `...-part1` of the root NVMe had passed every check).
        (
            &["/dev/disk/by-id/nvme-X-part1", "/srv/data", &hash],
            "names a partition",
        ),
        (
            &["/dev/disk/by-id/nvme-X-part12", "/srv/data", &hash],
            "names a partition",
        ),
        // The mount path is one new directory under /srv, nothing else
        // (finding 3): no traversal, no nesting, no other root.
        (&[by_id, "relative/path", &hash], "mount path must be"),
        (&[by_id, "/mnt/data", &hash], "mount path must be"),
        (&[by_id, "/", &hash], "mount path must be"),
        (&[by_id, "/srv", &hash], "mount path must be"),
        (&[by_id, "/srv/", &hash], "mount path must be"),
        (&[by_id, "/srv/.", &hash], "mount path must be"),
        (&[by_id, "/srv/..", &hash], "mount path must be"),
        (&[by_id, "/srv/../etc", &hash], "mount path must be"),
        (&[by_id, "/srv/a/b", &hash], "mount path must be"),
        (&[by_id, "/srv/.hidden", &hash], "mount path must be"),
        (&[by_id, "/srv/a b", &hash], "mount path must be"),
        // No approved plan: the write refuses before it resolves anything.
        (&[by_id, "/srv/data"], "plan-sha256"),
        // A hash that is not one — short, upper-case, or not hex.
        (&[by_id, "/srv/data", &hash[..63]], "64 lowercase hex"),
        (
            &[by_id, "/srv/data", &hash.to_uppercase()],
            "64 lowercase hex",
        ),
        (&[by_id, "/srv/data", &"g".repeat(64)], "64 lowercase hex"),
    ];
    // Under bash, as the runner runs it (its shebang): the write path
    // uses arrays and pipefail since the review of car 0556935a.
    let run = |args: &[&str]| {
        let out = Command::new("bash")
            .arg(&script)
            .args(args)
            .output()
            .expect("the script runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code(), text)
    };
    for (args, expected) in cases {
        let (code, text) = run(args);
        assert_eq!(
            code,
            Some(78),
            "a wrong request is EX_CONFIG, not a failed run: {args:?} -> {text}"
        );
        assert!(
            text.contains(expected),
            "the refusal says which precondition failed: {args:?} -> {text}"
        );
        // The plan refuses the same target in the same words: a plan is
        // rendered only for what the write would do.
        if args.len() == 3 && !expected.contains("hex") {
            let (code, text) = run(&["--plan", args[0], args[1]]);
            assert_eq!(code, Some(78), "--plan {args:?} -> {text}");
            assert!(text.contains(expected), "--plan {args:?} -> {text}");
        }
    }
}

/// AN EXIT 0 IS NOT AN EFFECT (backlog fdbb447e part 1, design
/// 3036296f mechanism B). The runner judges a verb's declared `effect`
/// once, on the run's own output, and records the verdict beside the
/// exit — so a reader of the step never takes `answered, exit 0` for
/// "it changed what it exists to change". The daily prune's proof read
/// the outcome alone and counted a refused run as proof.
///
/// One fixture verb per case, judged against its own `effect`:
///   - it printed its read-back line: `effect` is that line (the LAST
///     one matching, since a verb says its result last);
///   - it exited 0 and printed no such line: `effect_unproven` names the
///     declared regex, and `effect` is cleared;
///   - it FAILED: nothing is judged — the exit is already the verdict.
fn effect_fixture(case: &str, body: &str, spec_extra: &str) -> (PathBuf, PathBuf) {
    let root = scratch(case);
    stub_sor(&root);
    let verb = root.join("acts.sh");
    write_exec(&verb, body);
    let verbs = verbs_dir(
        &root,
        &[(
            "acts",
            &format!(
                r#"{{"about":"MUTATING — a fixture verb","hosts":["forge"],"argv":["{}"],"params":[]{spec_extra}}}"#,
                verb.display()
            ),
        )],
    );
    packet(&root, "acts", "[]");
    (root, verbs)
}

const EFFECT_RE: &str =
    r#","effect":"^acts: (deleted [0-9]+, re-listed [0-9]+ left|dry run: would delete [0-9]+)$""#;

/// Structured reads keep bytes apart; stderr is evidence, never JSON input.
#[test]
fn a_declared_structured_read_preserves_both_streams_without_interpreting_them() {
    assert!(has("jq"), "structured-result test requires jq");
    let (root, verbs) = effect_fixture(
        "separate-streams",
        "#!/bin/sh\nprintf '{\"ok\":true}\\n'\nprintf 'diagnostic\\n' >&2\n",
        r#", "capture":"separate-streams""#,
    );
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no receipt: {out}"));
    assert_eq!(md["exit_code"], "0", "{md}");
    assert_eq!(
        md["streams"]["stdout"]["base64"], "eyJvayI6dHJ1ZX0K",
        "{md}"
    );
    assert_eq!(md["streams"]["stdout"]["bytes"], 12, "{md}");
    assert_eq!(
        md["streams"]["stderr"]["base64"], "ZGlhZ25vc3RpYwo=",
        "{md}"
    );
    assert_eq!(md["streams"]["stderr"]["bytes"], 11, "{md}");
    assert_eq!(md["streams"]["stdout"]["complete"], true, "{md}");
    assert_eq!(md["streams"]["stderr"]["complete"], true, "{md}");
    assert!(md["output"].as_str().unwrap().contains("diagnostic"));
}

#[test]
fn a_capped_structured_read_names_the_lost_bytes_instead_of_claiming_completeness() {
    assert!(has("jq"), "structured-result test requires jq");
    let (root, verbs) = effect_fixture(
        "separate-streams-capped",
        "#!/bin/sh\nprintf 'abcdefgh'\nprintf 'diagnostic' >&2\nexit 7\n",
        r#", "capture":"separate-streams""#,
    );
    let (out, payload) = run(&root, &verbs, &[("OPS_OUTPUT_CAP", "4".into())]);
    let md = payload.unwrap_or_else(|| panic!("no receipt: {out}"));
    assert_eq!(md["exit_code"], "7", "{md}");
    assert_eq!(md["streams"]["stdout"]["bytes"], 8, "{md}");
    assert_eq!(md["streams"]["stdout"]["base64"], "YWJjZA==", "{md}");
    assert_eq!(md["streams"]["stdout"]["complete"], false, "{md}");
    assert_eq!(md["streams"]["stderr"]["complete"], false, "{md}");
}

#[test]
fn an_ordinary_read_clears_any_prior_structured_stream_record() {
    assert!(has("jq"), "structured-result test requires jq");
    let (root, verbs) = effect_fixture("ordinary-no-streams", "#!/bin/sh\necho ordinary\n", "");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no receipt: {out}"));
    assert!(
        md.get("streams").is_some(),
        "merge must explicitly clear prior streams: {md}"
    );
    assert_eq!(md["streams"], serde_json::Value::Null);
    assert_eq!(md["output"], "ordinary\n");
}

#[test]
fn a_failed_structured_stream_encoder_leaves_an_explicit_unread_result() {
    assert!(has("jq"), "structured-result test requires jq");
    let (root, verbs) = effect_fixture(
        "separate-streams-encoder-failed",
        "#!/bin/sh\necho actual-answer\n",
        r#", "capture":"separate-streams""#,
    );
    write_exec(&root.join("bin/base64"), "#!/bin/sh\nexit 8\n");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no receipt: {out}"));
    assert_eq!(md["exit_code"], "0", "verb result is conserved: {md}");
    assert_eq!(md["streams"], serde_json::Value::Null, "{md}");
    assert!(
        md["streams_unread"].as_str().is_some_and(|s| !s.is_empty()),
        "{md}"
    );
    assert_eq!(md["output"], "actual-answer\n");
}

#[test]
fn a_structured_read_conserves_binary_bytes_and_their_digest() {
    assert!(has("jq"), "structured-result test requires jq");
    let (root, verbs) = effect_fixture(
        "separate-streams-binary",
        "#!/bin/sh\nprintf '\\000\\377\\n\\n'\n",
        r#", "capture":"separate-streams""#,
    );
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no receipt: {out}"));
    assert_eq!(md["streams"]["stdout"]["base64"], "AP8KCg==", "{md}");
    assert_eq!(md["streams"]["stdout"]["bytes"], 4, "{md}");
    assert_eq!(md["streams"]["stdout"]["retained_bytes"], 4, "{md}");
    assert_eq!(md["streams"]["stdout"]["complete"], true, "{md}");
    let file = root.join("expected-bytes");
    std::fs::write(&file, [0, 255, 10, 10]).unwrap();
    let hash = Command::new("sha256sum").arg(&file).output().unwrap();
    assert!(hash.status.success());
    assert_eq!(
        md["streams"]["stdout"]["sha256"],
        String::from_utf8_lossy(&hash.stdout)[..64]
    );
}

#[test]
fn a_timed_out_structured_read_retains_its_partial_output_and_failed_exit() {
    assert!(has("jq"), "structured-result test requires jq");
    let (root, verbs) = effect_fixture(
        "separate-streams-timeout",
        "#!/bin/sh\nprintf 'partial'\nsleep 10\n",
        r#", "capture":"separate-streams", "timeout":1"#,
    );
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no receipt: {out}"));
    assert_eq!(md["exit_code"], "124", "{md}");
    assert_eq!(md["streams"]["stdout"]["base64"], "cGFydGlhbA==", "{md}");
    assert!(md["output"].as_str().unwrap().contains("command killed"));
}

#[test]
fn an_unknown_structured_capture_format_is_refused_before_the_verb_runs() {
    assert!(has("jq"), "structured-result test requires jq");
    let (root, verbs) = effect_fixture(
        "separate-streams-unknown",
        "#!/bin/sh\necho should-never-run\n",
        r#", "capture":"unknown""#,
    );
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no receipt: {out}"));
    assert_eq!(md["disposition"], "refused", "{md}");
    assert_eq!(md["streams"], serde_json::Value::Null);
    assert!(
        md["output"]
            .as_str()
            .unwrap()
            .contains("unsupported capture")
    );
    assert!(!md["output"].as_str().unwrap().contains("should-never-run"));
}

#[test]
fn the_explicit_volume_plan_declares_structured_stream_capture() {
    let spec: serde_json::Value = serde_json::from_slice(
        &std::fs::read(repo_root().join("infra/ops/verbs/plan-an-instance-volume-expansion.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(spec["capture"], "separate-streams");
    assert_ne!(spec["requires_approval"], true);
}

#[test]
fn full_sized_structured_streams_are_recorded_without_putting_bytes_on_argv() {
    assert!(has("jq"), "structured-result test requires jq");
    let (root, verbs) = effect_fixture(
        "separate-streams-full-sized",
        "#!/bin/sh\nhead -c 102400 /dev/zero\nhead -c 102400 /dev/zero >&2\n",
        r#", "capture":"separate-streams""#,
    );
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no receipt: {out}"));
    for stream in ["stdout", "stderr"] {
        assert_eq!(md["streams"][stream]["bytes"], 102400, "{stream}: {out}");
        assert_eq!(md["streams"][stream]["complete"], true, "{stream}: {out}");
        assert_eq!(
            md["streams"][stream]["base64"].as_str().unwrap().len(),
            136536
        );
    }
}

#[test]
fn a_declared_effect_the_run_printed_is_recorded_beside_its_exit() {
    needs_jq!();
    let (root, verbs) = effect_fixture(
        "effect-shown",
        "#!/bin/sh\necho 'acts: deleted 2, re-listed 9 left'\necho 'acts: deleting'\necho 'acts: deleted 3, re-listed 7 left'\n",
        EFFECT_RE,
    );
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["exit_code"], "0", "{md} / {out}");
    assert_eq!(
        md["effect"], "acts: deleted 3, re-listed 7 left",
        "the LAST matching line is the effect: {md}"
    );
    assert_eq!(md["effect_unproven"], serde_json::Value::Null, "{md}");
    assert_eq!(md["effect_unread"], serde_json::Value::Null, "{md}");
}

/// THE WHOLE OUTPUT IS JUDGED, NOT THE RECORDED COPY (the adversarial
/// review of this car). The runner keeps only the first OPS_OUTPUT_CAP
/// (100 KB) of a run's output, and a verb prints its read-back LAST —
/// every prune-registry-versions run and sweep-archive-branches 54547b33
/// (184 KB) were past the cap. Judged on the cut copy, each would have
/// been recorded EFFECT NOT SHOWN: a false statement in an immutable
/// record, for exactly the runs that did the most.
#[test]
fn an_effect_printed_past_the_output_cap_is_still_shown() {
    needs_jq!();
    let (root, verbs) = effect_fixture(
        "effect-past-cap",
        "#!/bin/sh\ni=0\nwhile [ $i -lt 1500 ]; do echo \"acts: deleting branch $i of a long list, padded so the listing outgrows the runner's output cap\"; i=$((i+1)); done\necho 'acts: deleted 1500, re-listed 0 left'\n",
        EFFECT_RE,
    );
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["exit_code"], "0", "{out}");
    let recorded = md["output"].as_str().unwrap_or_default();
    assert!(
        recorded.contains("[ops-runner: output truncated"),
        "the fixture must outgrow the cap, or it proves nothing: {} bytes recorded",
        recorded.len()
    );
    assert!(
        !recorded.contains("acts: deleted 1500"),
        "the effect line is past the cap in the recorded copy"
    );
    assert_eq!(
        md["effect"], "acts: deleted 1500, re-listed 0 left",
        "judged on the whole output: {}",
        md["effect_unproven"]
    );
    assert_eq!(md["effect_unproven"], serde_json::Value::Null);
}

/// The one line kept is bounded, as the output is: a verb that printed a
/// runaway line as its effect records 2,000 characters of it, not all.
#[test]
fn a_recorded_effect_line_is_capped() {
    needs_jq!();
    let (root, verbs) = effect_fixture(
        "effect-long-line",
        "#!/bin/sh\nprintf 'acts: dry run: would delete 3'\ni=0\nwhile [ $i -lt 500 ]; do printf '0123456789'; i=$((i+1)); done\necho\n",
        r#","effect":"^acts: dry run: would delete [0-9]+""#,
    );
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    let effect = md["effect"]
        .as_str()
        .unwrap_or_else(|| panic!("the line matched: {md}"));
    assert_eq!(effect.chars().count(), 2000, "{}", effect.len());
    assert!(effect.starts_with("acts: dry run: would delete 3"));
}

#[test]
fn an_exit_0_that_did_not_show_its_declared_effect_is_recorded_unproven() {
    needs_jq!();
    // The shape the prune's proof missed: the verb ran, said something,
    // exited 0 — and never printed the line that reads its effect back.
    let (root, verbs) = effect_fixture(
        "effect-missing",
        "#!/bin/sh\necho 'acts: REFUSED — the keep set could not be derived'\necho '  Nothing was written.'\nexit 0\n",
        EFFECT_RE,
    );
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert_eq!(md["exit_code"], "0", "{md} / {out}");
    assert_eq!(md["effect"], serde_json::Value::Null, "{md}");
    let why = md["effect_unproven"]
        .as_str()
        .unwrap_or_else(|| panic!("an exit 0 with no effect line must say so on the step: {md}"));
    assert!(
        why.contains("no line of the output matches") && why.contains("^acts: (deleted"),
        "the record names the regex it looked for: {why}"
    );
}

#[test]
fn a_declared_effect_that_cannot_be_judged_is_unproven_not_proven() {
    needs_jq!();
    // No evidence is not a pass: a regex the runner's engine cannot
    // compile is a run whose effect was NOT shown, never one that was.
    let (root, verbs) = effect_fixture(
        "effect-bad-regex",
        "#!/bin/sh\necho 'acts: deleted 1, re-listed 1 left'\n",
        r#","effect":"^acts: (deleted""#,
    );
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["exit_code"], "0", "{md} / {out}");
    assert_eq!(md["effect"], serde_json::Value::Null, "{md}");
    assert!(
        md["effect_unproven"]
            .as_str()
            .is_some_and(|w| w.contains("could not be judged")),
        "{md}"
    );
}

#[test]
fn a_failed_run_is_not_judged_for_its_effect() {
    needs_jq!();
    let (root, verbs) = effect_fixture(
        "effect-failed",
        "#!/bin/sh\necho 'acts: deleted 3, re-listed 7 left'\necho 'acts: FAILED — the re-list disagreed' >&2\nexit 1\n",
        EFFECT_RE,
    );
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["exit_code"], "1", "{md} / {out}");
    for key in ["effect", "effect_unproven", "effect_unread"] {
        assert_eq!(
            md[key],
            serde_json::Value::Null,
            "{key}: a failed run's exit is its verdict: {md}"
        );
    }
}

#[test]
fn a_verb_that_declares_no_read_back_says_so_on_every_clean_run() {
    needs_jq!();
    // Loud, not refused (DR rule 62dac114): the verb still runs and
    // still answers; its record says that exit 0 is all this run proves.
    let (root, verbs) = effect_fixture(
        "effect-unread",
        "#!/bin/sh\necho 'acts: done'\n",
        r#","effect_unread":"prints no re-list after it deletes (follow-up: re-list and print what is left)""#,
    );
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["exit_code"], "0", "{md} / {out}");
    assert_eq!(
        md["effect_unread"],
        "prints no re-list after it deletes (follow-up: re-list and print what is left)",
        "{md}"
    );
    assert_eq!(md["effect"], serde_json::Value::Null, "{md}");
    assert_eq!(md["effect_unproven"], serde_json::Value::Null, "{md}");
}
