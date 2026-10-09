//! THE ADMISSION-SOURCE DISCOVERY VERB PRESENTS THE MACHINE TOKEN ON ITS
//! ONE READ OF THE ESTATE REGISTRY, AND IS NEVER STOPPED BY IT (design
//! 6805c764; backlog 44b2087e, the owed roster).
//!
//! `infra/forge/discover-admission-source.sh` sends one request to a gated
//! port: `GET $BOSS_JOBS_URL/api/estate/nodes`, signed with `x-boss-user`
//! alone. It runs on the forge under the ops runner (root,
//! `RuntimeDirectory=boss-ops-runner`), where the token is readable.
//!
//! ITS TWIN IS NOT HERE. `probe-admission-source-access.sh` sends the same
//! read, and is the one of the two on the live record (1 would-refuse
//! fact, 2026-10-05). Its bytes and its sourced files are a bound method
//! of design f927de0c (infra/forge/admission-reader-identity.py METHODS),
//! so stamping it is that design's car; the sender pin's NOT_YET entry
//! says what removes it. `VERBS` is a list so that car adds one row.
//!
//! The script is RUN here against a stub `curl` and a stub `sudo`, with
//! every state of the mount:
//!
//!   absent     no slot — the read is exactly the one it was
//!   present    a slot, the registry's host on the list — the read carries
//!              a 0600 header file holding the slot's value
//!   off-host   a slot, the registry's host NOT on the list — withheld
//!   line-break a slot no header can carry — withheld
//!
//! and in every one the verb's answer, exit code and EMPTY stderr are what
//! a host with no token gets. Empty, because the ops runner folds a verb's
//! stderr into its receipt (`> "$rawf" 2>&1`) and this verb promises that
//! no diagnostic text reaches one; so the reader's own "withheld" line is
//! silenced here, unlike in every other stamped sender.
//!
//! THE HEADER FILE'S LIFE IS THE ONE REQUEST. The read is followed by a
//! privileged container that may run for fourteen minutes. The stub `sudo`
//! looks for the header file the stub `curl` was handed and finds it GONE:
//! the script closes the reader's directory right after the read, so a
//! verb killed inside the container — the long part, and the part the
//! runner's own timeout kills — leaves nothing. Nothing is left under
//! TMPDIR on any exit either.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::process::Command;

const TOKEN: &str = "synthetic-machine-fixture";

struct Verb {
    script: &'static str,
    signer: &'static str,
    /// A report the verb's own sanitizer accepts, answered with exit 4.
    report: &'static str,
}

const VERBS: &[Verb] = &[Verb {
    script: "infra/forge/discover-admission-source.sh",
    signer: "automation:discover-admission-source",
    report: r#"{"schema":"boss.admission-source-discovery.v1","scope":"configuration_discovery_only","history_verdict":"unavailable","roster":{"state":"unavailable","reason":"native_read_failed"},"nodes":[]}"#,
}];

struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// `mode=<octal>` and the file's one line, per header file handed over.
    headers: String,
    /// Every argv the stub curl was run with, one line each.
    argv: String,
    /// What the stub sudo found where the header file had been.
    at_container: String,
    /// Where the header file handed to curl had been.
    header_path: String,
    /// What is left under TMPDIR and the runtime directory after the
    /// verb exits.
    left: Vec<String>,
    run_dir: String,
}

fn go(verb: &Verb, tag: &str, slot: Option<&str>, hosts: &str, curl_rc: &str) -> Outcome {
    go_in(verb, tag, slot, hosts, curl_rc, false)
}

/// `runtime`: hand the verb a unit's `RUNTIME_DIRECTORY`, as the ops
/// runner's unit does.
fn go_in(
    verb: &Verb,
    tag: &str,
    slot: Option<&str>,
    hosts: &str,
    curl_rc: &str,
    runtime: bool,
) -> Outcome {
    let root = repo_root();
    let name = verb.script.rsplit('/').next().unwrap();
    let scratch = scratch_dir(&format!("admission-token-{name}-{tag}"));
    let (bin, mount, tmp, run) = (
        scratch.join("bin"),
        scratch.join("mount"),
        scratch.join("tmp"),
        scratch.join("run"),
    );
    for d in [&bin, &mount, &tmp, &run] {
        create_dir(d);
    }
    if let Some(value) = slot {
        write_file(&mount.join("current"), value);
    }
    write_file(&scratch.join("estate.json"), r#"{"data":[]}"#);
    write_file(&scratch.join("answer.json"), verb.report);
    // The request the script sends, answered the way `-w '\n%{http_code}'`
    // answers: the body, a newline, the status. The header FILE is read
    // here, while it exists; its path is kept for the stub sudo.
    write_exec(
        &bin.join("curl"),
        r#"#!/bin/sh
# One line per run: the `-w` format holds a newline of its own.
{ printf '%s' "$*" | tr '\n' ' '; echo; } >> "$ARGV"
prev=
for a in "$@"; do
    if [ "$prev" = -H ]; then
        case "$a" in
            @*)
                { stat -c 'mode=%a' "${a#@}"; cat "${a#@}"; } >> "$HEADERS"
                printf '%s' "${a#@}" > "$HEADER_PATH"
                ;;
        esac
    fi
    prev="$a"
done
[ "${CURL_RC:-0}" = 0 ] || exit "$CURL_RC"
cat "$ESTATE"
printf '\n200'
"#,
    );
    write_exec(
        &bin.join("sudo"),
        r#"#!/bin/sh
case "$*" in
 '-n docker image inspect '*|'-n docker create '*) exit 0 ;;
 '-n docker container inspect '*) exit 1 ;;
esac
if [ -s "$HEADER_PATH" ]; then
    if [ -e "$(cat "$HEADER_PATH")" ]; then echo present; else echo gone; fi > "$AT_CONTAINER"
else
    echo none > "$AT_CONTAINER"
fi
cat > /dev/null
cat "$ANSWER"
exit 4
"#,
    );
    let mut cmd = Command::new("bash");
    cmd.arg(root.join(verb.script))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("BOSS_SOR_ENV", scratch.join("absent.env"))
        .env("BOSS_ESTATE_NODES_URL", "http://jobs.test/api/estate/nodes")
        .env("BOSS_FORGE_REGISTRY_HOST", "registry.invalid")
        .env("BOSS_MACHINE_TOKEN_DIR", &mount)
        .env("BOSS_MACHINE_TOKEN_HOSTS", hosts)
        .env("TMPDIR", &tmp)
        .env_remove("RUNTIME_DIRECTORY")
        .env("ARGV", scratch.join("argv"))
        .env("HEADERS", scratch.join("headers"))
        .env("HEADER_PATH", scratch.join("header-path"))
        .env("AT_CONTAINER", scratch.join("at-container"))
        .env("ESTATE", scratch.join("estate.json"))
        .env("ANSWER", scratch.join("answer.json"))
        .env("CURL_RC", curl_rc);
    if runtime {
        cmd.env("RUNTIME_DIRECTORY", &run);
    }
    let out = cmd.output().unwrap();
    let read = |f: &str| std::fs::read_to_string(scratch.join(f)).unwrap_or_default();
    Outcome {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        headers: read("headers"),
        argv: read("argv"),
        at_container: read("at-container").trim().to_string(),
        header_path: read("header-path"),
        left: [&tmp, &run]
            .iter()
            .flat_map(|d| std::fs::read_dir(d).unwrap())
            .filter_map(Result::ok)
            .map(|e| e.path().display().to_string())
            .collect(),
        run_dir: run.display().to_string(),
    }
}

#[test]
fn the_registry_read_presents_the_machine_token_and_is_never_stopped_by_it() {
    let fixture = format!("{TOKEN}\n");
    let broken = format!("{TOKEN}\nsecond-line\n");
    for verb in VERBS {
        let absent = go(verb, "absent", None, "jobs.test", "0");
        assert_eq!(absent.code, Some(4), "{}: {}", verb.script, absent.stdout);
        assert_eq!(absent.stdout.trim(), verb.report, "{}", verb.script);
        assert_eq!(absent.stderr, "", "{}", verb.script);
        assert_eq!(
            absent.headers, "",
            "{}: no token, and a header file was handed over",
            verb.script
        );
        assert_eq!(
            absent.at_container, "none",
            "{}: no header file was ever made",
            verb.script
        );
        assert_eq!(absent.argv.lines().count(), 1, "one read: {}", absent.argv);
        assert!(
            absent.argv.contains(verb.signer),
            "{}: the read is still signed: {}",
            verb.script,
            absent.argv
        );

        for (tag, slot, hosts, carries) in [
            ("present", fixture.as_str(), "jobs.test", true),
            ("off-host", fixture.as_str(), "elsewhere.test", false),
            ("line-break", broken.as_str(), "jobs.test", false),
        ] {
            let r = go(verb, tag, Some(slot), hosts, "0");
            let at = format!("{} {tag}", verb.script);
            assert_eq!(r.code, absent.code, "{at}: the exit is unchanged");
            assert_eq!(r.stdout, absent.stdout, "{at}: the answer is unchanged");
            assert_eq!(
                r.stderr, "",
                "{at}: nothing reaches the ops receipt but the report"
            );
            assert_eq!(
                r.argv.lines().count(),
                1,
                "{at}: one read, once: {}",
                r.argv
            );
            assert!(
                !r.argv.contains(TOKEN) && !r.stdout.contains(TOKEN),
                "{at}: the token is in curl's argv or in the answer"
            );
            if carries {
                assert_eq!(
                    r.headers,
                    format!("mode=600\nx-boss-machine-token: {TOKEN}\n"),
                    "{at}: the read carries the token as a 0600 header file"
                );
                assert_eq!(
                    r.at_container, "gone",
                    "{at}: the header file outlived its one request into the container run"
                );
            } else {
                assert_eq!(r.headers, "", "{at}: this read goes out without the token");
            }
            assert!(
                r.left.is_empty(),
                "{at}: the header directory outlived the verb: {:?}",
                r.left
            );
        }

        // The read itself fails: the verb answers `estate_unavailable`
        // as it always did, and the header directory still goes.
        let failed = go(verb, "read-fails", Some(&fixture), "jobs.test", "7");
        let at = format!("{} read-fails", verb.script);
        assert_eq!(failed.code, Some(4), "{at}");
        assert!(
            failed.stdout.contains("\"reason\":\"estate_unavailable\""),
            "{at}: {}",
            failed.stdout
        );
        assert_eq!(failed.stderr, "", "{at}");
        assert_eq!(
            failed.headers,
            format!("mode=600\nx-boss-machine-token: {TOKEN}\n"),
            "{at}: the failed read had carried the token"
        );
        assert!(failed.left.is_empty(), "{at}: {:?}", failed.left);
    }
}

/// With a unit's `RuntimeDirectory=` in the environment — the ops runner's
/// (`RuntimeDirectory=boss-ops-runner`), which a verb inherits — the file
/// is made THERE, on the tmpfs systemd removes when the unit stops, after
/// a SIGKILL too; never under /tmp.
#[test]
fn under_the_runners_runtime_directory_the_header_file_is_made_there() {
    for verb in VERBS {
        let r = go_in(verb, "runtime", Some(TOKEN), "jobs.test", "0", true);
        assert_eq!(r.code, Some(4), "{}: {}", verb.script, r.stdout);
        assert!(
            r.header_path
                .starts_with(&format!("{}/boss-secret-header.", r.run_dir)),
            "{}: the header file was made at {:?}, not under the unit's runtime directory {}",
            verb.script,
            r.header_path,
            r.run_dir
        );
        assert!(
            r.left.is_empty(),
            "{}: and it is gone when the verb exits: {:?}",
            verb.script,
            r.left
        );
    }
}
