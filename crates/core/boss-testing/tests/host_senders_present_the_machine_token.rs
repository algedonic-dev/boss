//! The off-cluster host senders PRESENT the estate machine token and are
//! never stopped by it (design 6805c764; backlog 2710c8fc).
//!
//! MEASURED 2026-10-06. Every service's machine gate was in `report`, and
//! the 72-hour window held thousands of `machine_gate.would_refuse`
//! facts from the forge host and boss-gcp. Most host scripts already
//! took the token through the one shell reader
//! (`infra/lib/secret-header.sh machine_token_header`); seven send sites
//! in six libraries, plus the workflow publish's live read, sent
//! `x-boss-user` — or nothing — with no reader call at all:
//!
//!   * `infra/estate/observe-lib.sh` `post_observation` (the host,
//!     nodefs, volume and door observers);
//!   * `infra/estate/observe-units.sh` (the unit observer's POST);
//!   * `infra/estate/node-roles.sh` `read_node_roles` (both converges);
//!   * `infra/forge/cluster-node-lib.sh` `node_read_registry` (the node
//!     verbs);
//!   * `infra/forge/cluster-deploy-lib.sh` `answer_converge_requests`
//!     (the converge's annotation, sent from its EXIT trap);
//!   * `infra/forge/alert-lib.sh` `alert_post` and `alert_owner` (the
//!     watchdog's alert, and the converge's through `tenant_check_alert`);
//!   * `infra/gcp/publish-workflow.sh` `read_live` — run in
//!     `publish_workflow_sh.rs`, beside the stub CLI that verb needs to
//!     reach its read, and held by name in
//!     `every_shell_identity_sender_presents_the_machine_token.rs`.
//!
//! THE BAR. These are the watchdog, converge, alert and observer paths —
//! the loops that must owe nothing to what they watch (CLAUDE.md, "an
//! arm that needs the patient is not an arm"). So each site is run here,
//! against a stub `curl`, in every state a host can be in:
//!
//!   absent      no slot at all — every host today, until a delivery
//!   present     a slot, and the system of record on the host list
//!   off-host    a slot, and the system of record NOT on the host list
//!   line-break  a slot no header can carry
//!   not-a-file  `current` is a directory
//!
//! and in two shells of the caller: its own (`top`) and a `( … )`
//! subshell (`sub`) — the shape that held every train on 2026-10-01,
//! when a lint's first reader call sat inside `$( ( … ) )` and the
//! reader's refusal became the lint's verdict the moment a token
//! appeared (backlog 920524dc). For every cell:
//!
//!   * the exit status and stdout are EXACTLY the absent run's — the
//!     real work is the same work;
//!   * the request still goes out;
//!   * it carries the token only in `present`, and then as a 0600 header
//!     FILE (never argv), holding the slot's value and nothing else. In
//!     a subshell it either carries it or the reader says, on stderr,
//!     that it refused a first call there — never silence, never a stop;
//!   * the caller's own EXIT trap still runs;
//!   * no header directory outlives the process;
//!   * the fixture token appears in no argv, stdout or stderr.
//!
//! The token here is the literal `synthetic-machine-fixture`; no test
//! reads a mount, and every run names its own empty or planted
//! `BOSS_MACHINE_TOKEN_DIR`.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const TOKEN: &str = "synthetic-machine-fixture";
const SOR: &str = "http://sor.test:7900";

/// The stub `curl`. Logs its argv, copies every header FILE it was
/// handed (mode, then content) into `$HEADER_LOG`, drains a piped body,
/// and answers `$STUB_PEOPLE` to the people registry and `$STUB_REPLY`
/// to everything else — or fails like a dark API under `STUB_DARK=1`.
const CURL: &str = r#"#!/usr/bin/env bash
printf '%s\n' "curl $*" >> "$STUB_LOG"
piped=0
for a in "$@"; do
    case "$a" in
        @-) piped=1 ;;
        @*)
            f=${a#@}
            if [ -f "$f" ]; then
                { stat -c 'mode=%a' "$f"; cat "$f"; } >> "$HEADER_LOG"
            else
                echo "missing header file" >> "$HEADER_LOG"
            fi
            ;;
    esac
done
[ "$piped" = 1 ] && cat > /dev/null
[ "${STUB_DARK:-0}" = 1 ] && exit 7
for a in "$@"; do
    case "$a" in
        */api/people*) printf '%b' "${STUB_PEOPLE:-[]}"; exit 0 ;;
    esac
done
printf '%b' "$STUB_REPLY"
"#;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Slot {
    Absent,
    Present,
    OffHost,
    LineBreak,
    NotAFile,
}

const SLOTS: [Slot; 5] = [
    Slot::Absent,
    Slot::Present,
    Slot::OffHost,
    Slot::LineBreak,
    Slot::NotAFile,
];

struct Run {
    rc: i32,
    stdout: String,
    stderr: String,
    argv: String,
    headers: String,
    leaked: Vec<String>,
}

struct Bench {
    dir: PathBuf,
}

impl Bench {
    fn new(tag: &str) -> Bench {
        let dir = scratch_dir(&format!("host-senders-{tag}"));
        create_dir(&dir.join("bin"));
        write_exec(&dir.join("bin/curl"), CURL);
        Bench { dir }
    }

    /// Run `driver` under `shell` with the mount in state `slot`.
    fn run(&self, shell: &str, driver: &Path, slot: Slot, env: &[(&str, &str)]) -> Run {
        let cell = self.dir.join(format!("cell-{slot:?}"));
        let _ = std::fs::remove_dir_all(&cell);
        create_dir(&cell);
        let mount = cell.join("mount");
        create_dir(&mount);
        match slot {
            Slot::Absent => {}
            Slot::Present | Slot::OffHost => write_file(&mount.join("current"), TOKEN),
            Slot::LineBreak => write_file(&mount.join("current"), "synthetic\nmachine-fixture"),
            Slot::NotAFile => create_dir(&mount.join("current")),
        }
        let tmp = cell.join("tmp");
        create_dir(&tmp);
        let log = cell.join("argv.log");
        let headers = cell.join("header.log");
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new(shell);
        cmd.arg(driver)
            .current_dir(&cell)
            .env("PATH", path)
            .env("TMPDIR", &tmp)
            .env("STUB_LOG", &log)
            .env("HEADER_LOG", &headers)
            .env("REPO", repo_root())
            .env("CELL", &cell)
            .env("BOSS_MACHINE_TOKEN_DIR", &mount)
            .env(
                "BOSS_MACHINE_TOKEN_HOSTS",
                if slot == Slot::OffHost {
                    "elsewhere.test"
                } else {
                    "sor.test"
                },
            )
            .env("BOSS_SOR_ENV", cell.join("no-sor.env"))
            .env("BOSS_JOBS_URL", SOR)
            .env("JOBS_API", SOR)
            .env_remove("RUNTIME_DIRECTORY")
            .env_remove("BOSS_PLATFORM_OWNER")
            .env_remove("BOSS_NODE_ROLES")
            .env_remove("BOSS_ESTATE_NODES_URL");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run the driver");
        let leaked = std::fs::read_dir(&tmp)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with("boss-secret-header."))
                    .collect()
            })
            .unwrap_or_default();
        Run {
            rc: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            argv: std::fs::read_to_string(&log).unwrap_or_default(),
            headers: std::fs::read_to_string(&headers).unwrap_or_default(),
            leaked,
        }
    }
}

/// What one sender must do in every cell of the matrix.
struct Site<'a> {
    name: &'a str,
    shell: &'a str,
    /// The driver for the caller's own shell, and for a `( … )` subshell.
    top: &'a Path,
    sub: &'a Path,
    env: &'a [(&'a str, &'a str)],
    /// What every request this site sends names in its argv.
    sends: &'a [&'a str],
    /// How many requests carry the token when it is present (top shell).
    stamped: usize,
    /// What stdout says when the real work was done.
    did: &'a [&'a str],
}

fn judge(bench: &Bench, site: &Site) {
    for (shape, driver) in [("top", site.top), ("sub", site.sub)] {
        let base = bench.run(site.shell, driver, Slot::Absent, site.env);
        for want in site.did {
            assert!(
                base.stdout.contains(want),
                "{} [{shape}/absent]: the real work is done with no token at all — stdout lacks {want:?}\nstdout:\n{}\nstderr:\n{}",
                site.name,
                base.stdout,
                base.stderr
            );
        }
        for slot in SLOTS {
            let run = bench.run(site.shell, driver, slot, site.env);
            let cell = format!("{} [{shape}/{slot:?}]", site.name);
            assert_eq!(
                run.rc, base.rc,
                "{cell}: the token changed the exit status\nstdout:\n{}\nstderr:\n{}",
                run.stdout, run.stderr
            );
            assert_eq!(
                run.stdout, base.stdout,
                "{cell}: the token changed what the caller did\nstderr:\n{}",
                run.stderr
            );
            for want in site.sends {
                assert!(
                    run.argv.contains(want),
                    "{cell}: the request to {want} did not go out\nargv:\n{}\nstderr:\n{}",
                    run.argv,
                    run.stderr
                );
            }
            for (what, text) in [
                ("argv", &run.argv),
                ("stdout", &run.stdout),
                ("stderr", &run.stderr),
            ] {
                assert!(
                    !text.contains(TOKEN) && !text.contains("machine-fixture"),
                    "{cell}: the token reached {what}:\n{text}"
                );
            }
            assert!(
                run.leaked.is_empty(),
                "{cell}: a header directory outlived the run: {:?}",
                run.leaked
            );
            assert!(
                !run.headers.contains("missing header file"),
                "{cell}: curl was handed a header file that was not there:\n{}",
                run.headers
            );
            let carried = run
                .headers
                .matches(&format!("x-boss-machine-token: {TOKEN}\n"))
                .count();
            if slot != Slot::Present {
                assert_eq!(
                    carried, 0,
                    "{cell}: a token went out that should not have:\n{}",
                    run.headers
                );
                assert!(
                    !run.argv.contains(" -H @"),
                    "{cell}: no header file is offered without a token to carry:\n{}",
                    run.argv
                );
                continue;
            }
            if shape == "top" {
                assert_eq!(
                    carried, site.stamped,
                    "{cell}: {} request(s) carry the token from the caller's own shell\nheaders:\n{}\nargv:\n{}\nstderr:\n{}",
                    site.stamped, run.headers, run.argv, run.stderr
                );
            } else {
                assert!(
                    carried == site.stamped || run.stderr.contains("inside a subshell"),
                    "{cell}: in a subshell the request is stamped, or the reader says why it is not — never silence\nheaders:\n{}\nstderr:\n{}",
                    run.headers,
                    run.stderr
                );
            }
            if carried > 0 {
                assert_eq!(
                    run.headers.matches("mode=600\n").count(),
                    carried,
                    "{cell}: every header file is 0600:\n{}",
                    run.headers
                );
            }
        }
    }
}

/// A scratch tree `infra/{estate,lib}` holding copies of the real
/// observer lib and (unless `without_reader`) the real reader — the
/// observer finds the reader beside `$0`, so its driver has to stand
/// where an observer stands.
fn estate_tree(bench: &Bench, name: &str, without_reader: bool) -> PathBuf {
    let root = bench.dir.join(name);
    create_dir(&root.join("infra/estate"));
    create_dir(&root.join("infra/lib"));
    let copy = |rel: &str| {
        let body = std::fs::read_to_string(repo_root().join(rel)).unwrap();
        write_file(&root.join(rel), &body);
    };
    copy("infra/estate/observe-lib.sh");
    if !without_reader {
        copy("infra/lib/secret-header.sh");
    }
    root
}

const OBSERVE_TOP: &str = r#"#!/bin/sh
set -eu
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"; echo caller-trap-ran' EXIT
SPOOL_DIR="$CELL/spool"
. "$(dirname "$0")/observe-lib.sh"
if post_observation '{"observed_at":"2026-10-06T00:00:00Z","scope":"host"}'; then
    echo posted
else
    echo kept
fi
"#;

const OBSERVE_SUB: &str = r#"#!/bin/sh
set -eu
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"; echo caller-trap-ran' EXIT
SPOOL_DIR="$CELL/spool"
. "$(dirname "$0")/observe-lib.sh"
if ( post_observation '{"observed_at":"2026-10-06T00:00:00Z","scope":"host"}' ); then
    echo posted
else
    echo kept
fi
"#;

#[test]
fn the_host_observers_post_with_or_without_a_token() {
    let bench = Bench::new("observe-lib");
    let root = estate_tree(&bench, "tree", false);
    let top = root.join("infra/estate/driver-top.sh");
    let sub = root.join("infra/estate/driver-sub.sh");
    write_exec(&top, OBSERVE_TOP);
    write_exec(&sub, OBSERVE_SUB);
    for shell in ["sh", "bash"] {
        judge(
            &bench,
            &Site {
                name: "observe-lib.sh post_observation",
                shell,
                top: &top,
                sub: &sub,
                env: &[("STUB_REPLY", "{\"accepted\":true}\\n202")],
                sends: &["/api/estate/observation"],
                stamped: 1,
                did: &["jobs api: 202", "posted", "caller-trap-ran"],
            },
        );
    }
}

/// `.` on a missing file ENDS a dash script. An observer whose checkout
/// lacks the reader must still observe — with a slot on the host too.
#[test]
fn an_observer_with_no_reader_beside_it_still_posts() {
    let bench = Bench::new("observe-lib-no-reader");
    let root = estate_tree(&bench, "tree", true);
    let top = root.join("infra/estate/driver-top.sh");
    write_exec(&top, OBSERVE_TOP);
    for shell in ["sh", "bash"] {
        for slot in [Slot::Absent, Slot::Present] {
            let run = bench.run(
                shell,
                &top,
                slot,
                &[("STUB_REPLY", "{\"accepted\":true}\\n202")],
            );
            assert_eq!(run.rc, 0, "{shell}/{slot:?}: {}", run.stderr);
            assert!(
                run.stdout.contains("posted"),
                "{shell}/{slot:?}: {}",
                run.stdout
            );
            assert!(
                !run.argv.contains(" -H @"),
                "{shell}/{slot:?}: {}",
                run.argv
            );
        }
    }
}

/// A dark API keeps the reading whatever the token's state: the spool is
/// the observer's whole point, and a header must not change it.
#[test]
fn a_dark_api_keeps_the_reading_with_or_without_a_token() {
    let bench = Bench::new("observe-lib-dark");
    let root = estate_tree(&bench, "tree", false);
    let top = root.join("infra/estate/driver-top.sh");
    write_exec(&top, OBSERVE_TOP);
    for slot in SLOTS {
        let run = bench.run("sh", &top, slot, &[("STUB_DARK", "1"), ("STUB_REPLY", "")]);
        assert_eq!(run.rc, 0, "{slot:?}: {}", run.stderr);
        assert!(run.stdout.contains("kept"), "{slot:?}: {}", run.stdout);
        assert!(run.leaked.is_empty(), "{slot:?}: {:?}", run.leaked);
    }
}

const SYSTEMCTL: &str = r#"#!/bin/sh
case "$1" in
    show) printf 'LoadState=loaded\nActiveState=inactive\nSubState=dead\nResult=success\nExecMainStatus=0\nType=oneshot\n' ;;
esac
exit 0
"#;

/// The unit observer is a whole script, not a lib: it is run as the hand
/// run (`UNITS=`) its header documents, against stub systemctl and
/// journalctl. It has no subshell caller, so both drivers are the script.
#[test]
fn the_unit_observer_posts_with_or_without_a_token() {
    let bench = Bench::new("observe-units");
    write_exec(&bench.dir.join("bin/systemctl"), SYSTEMCTL);
    write_exec(&bench.dir.join("bin/journalctl"), "#!/bin/sh\nexit 0\n");
    let script = repo_root().join("infra/estate/observe-units.sh");
    judge(
        &bench,
        &Site {
            name: "observe-units.sh",
            shell: "sh",
            top: &script,
            sub: &script,
            env: &[
                ("STUB_REPLY", "{\"accepted\":true}\\n202"),
                ("HOST_ID", "fixture-host"),
                ("UNITS", "boss-fixture.service"),
                ("BOSS_NODE_ROLES", "ops-runner"),
            ],
            sends: &["/api/estate/observation"],
            stamped: 1,
            did: &["jobs api: 202"],
        },
    );
}

const ROLES_TOP: &str = r#"#!/usr/bin/env bash
set -uo pipefail
trap 'echo caller-trap-ran' EXIT
. "$REPO/infra/estate/node-roles.sh"
BOSS_CONVERGE_NAME=fixture-converge read_node_roles forge
echo "roles=$BOSS_NODE_ROLES source=$BOSS_NODE_ROLES_SOURCE"
"#;

const ROLES_SUB: &str = r#"#!/usr/bin/env bash
set -uo pipefail
trap 'echo caller-trap-ran' EXIT
. "$REPO/infra/estate/node-roles.sh"
(
    BOSS_CONVERGE_NAME=fixture-converge read_node_roles forge
    echo "roles=$BOSS_NODE_ROLES source=$BOSS_NODE_ROLES_SOURCE"
)
"#;

#[test]
fn a_converge_reads_its_roles_with_or_without_a_token() {
    let bench = Bench::new("node-roles");
    let top = bench.dir.join("roles-top.sh");
    let sub = bench.dir.join("roles-sub.sh");
    write_exec(&top, ROLES_TOP);
    write_exec(&sub, ROLES_SUB);
    let cache = bench.dir.join("roles.cache");
    judge(
        &bench,
        &Site {
            name: "node-roles.sh read_node_roles",
            shell: "bash",
            top: &top,
            sub: &sub,
            env: &[
                (
                    "STUB_REPLY",
                    "{\"data\":[{\"id\":\"forge\",\"roles\":[\"ops-runner\",\"forge\"]}]}\\n200",
                ),
                ("BOSS_NODE_ROLES_CACHE", cache.to_str().unwrap()),
            ],
            sends: &["/api/estate/nodes"],
            stamped: 1,
            did: &["roles=ops-runner,forge source=registry", "caller-trap-ran"],
        },
    );
}

const NODE_TOP: &str = r#"#!/usr/bin/env bash
set -euo pipefail
ME=fixture-node-verb
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"; echo caller-trap-ran' EXIT
. "$REPO/infra/forge/cluster-node-lib.sh"
node_read_registry w-1
echo "address=$NODE_ADDRESS role=$NODE_ROLE"
"#;

const NODE_SUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
ME=fixture-node-verb
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"; echo caller-trap-ran' EXIT
. "$REPO/infra/forge/cluster-node-lib.sh"
(
    node_read_registry w-1
    echo "address=$NODE_ADDRESS role=$NODE_ROLE"
)
"#;

#[test]
fn a_node_verb_reads_the_registry_with_or_without_a_token() {
    let bench = Bench::new("cluster-node-lib");
    let top = bench.dir.join("node-top.sh");
    let sub = bench.dir.join("node-sub.sh");
    write_exec(&top, NODE_TOP);
    write_exec(&sub, NODE_SUB);
    judge(
        &bench,
        &Site {
            name: "cluster-node-lib.sh node_read_registry",
            shell: "bash",
            top: &top,
            sub: &sub,
            env: &[(
                "STUB_REPLY",
                "{\"data\":[{\"id\":\"w-1\",\"retired\":false,\"role\":\"talos-worker\",\"address\":\"10.0.0.9\"}]}\\n200",
            )],
            sends: &["/api/estate/nodes"],
            stamped: 1,
            did: &["address=10.0.0.9 role=talos-worker", "caller-trap-ran"],
        },
    );
}

/// The watchdog's shape: its own trap first, the lib sourced at the top,
/// an alert raised from its own shell, then the replay.
const ALERT_TOP: &str = r#"#!/usr/bin/env bash
set -uo pipefail
marker=$(mktemp)
trap 'rm -f "$marker"; echo caller-trap-ran' EXIT
export ALERT_SPOOL="$CELL/spool"
. "$REPO/infra/forge/alert-lib.sh"
alert "fixture alert" "a detail" 2>> "$CELL/alert.err"
echo "waiting=$(alert_count)"
alert_replay 2>> "$CELL/alert.err" || true
echo "after-replay=$(alert_count)"
cat "$CELL/alert.err" | grep -v '^ALERT:\|^alert:' >&2 || true
"#;

/// The converge's shape (`tenant_check_alert` before this car): the lib
/// sourced AND used inside `( … )`, nothing opened by the parent.
const ALERT_SUB: &str = r#"#!/usr/bin/env bash
set -uo pipefail
marker=$(mktemp)
trap 'rm -f "$marker"; echo caller-trap-ran' EXIT
export ALERT_SPOOL="$CELL/spool"
(
    . "$REPO/infra/forge/alert-lib.sh"
    alert "fixture alert" "a detail" 2>> "$CELL/alert.err"
    echo "waiting=$(alert_count)"
) || echo "the alert door refused ($?)"
cat "$CELL/alert.err" | grep -v '^ALERT:\|^alert:' >&2 || true
"#;

#[test]
fn an_alert_is_filed_with_or_without_a_token() {
    let bench = Bench::new("alert-lib");
    let top = bench.dir.join("alert-top.sh");
    let sub = bench.dir.join("alert-sub.sh");
    write_exec(&top, ALERT_TOP);
    write_exec(&sub, ALERT_SUB);
    judge(
        &bench,
        &Site {
            name: "alert-lib.sh alert",
            shell: "bash",
            top: &top,
            sub: &sub,
            env: &[("STUB_REPLY", "201")],
            // The owner read on the people port and the alert's POST on
            // the jobs port: both are the system of record's host.
            sends: &[
                "/api/people?role=platform-admin",
                "http://sor.test:7900/api/jobs",
            ],
            stamped: 2,
            did: &["waiting=0", "caller-trap-ran"],
        },
    );
}

/// The alert that matters most is "the system of record is dark": it is
/// KEPT, in every token state, and a header never turns that into a stop.
#[test]
fn a_dark_api_keeps_the_alert_with_or_without_a_token() {
    let bench = Bench::new("alert-lib-dark");
    let top = bench.dir.join("alert-top.sh");
    write_exec(&top, ALERT_TOP);
    for slot in SLOTS {
        let run = bench.run(
            "bash",
            &top,
            slot,
            &[("STUB_DARK", "1"), ("STUB_REPLY", "")],
        );
        assert_eq!(run.rc, 0, "{slot:?}: {}", run.stderr);
        assert!(
            run.stdout.contains("waiting=1") && run.stdout.contains("after-replay=1"),
            "{slot:?}: the alert is kept, and stays kept through a failed replay:\n{}",
            run.stdout
        );
        assert!(
            run.stdout.contains("caller-trap-ran"),
            "{slot:?}: {}",
            run.stdout
        );
        assert!(run.leaked.is_empty(), "{slot:?}: {:?}", run.leaked);
    }
}

/// The cluster converge's shape, both sends in one run: an alert through
/// `tenant_check_alert` mid-run, and the annotation of the ops-request
/// that started it from INSIDE the EXIT trap — where the reader's own
/// cleanup has already run and no further trap can.
const DEPLOY: &str = r#"#!/usr/bin/env bash
set -euo pipefail
FAKE="$CELL/repo"
mkdir -p "$FAKE/.git"
printf '%s\n' 0a1b2c3d-0000-4000-8000-00000000abcd > "$FAKE/.git/boss-converge-requests.run"
export ALERT_SPOOL="$CELL/spool"
_finish() {
    local rc=$?
    answer_converge_requests "$FAKE" converge_failed "fixture-stage (exit $rc)" || true
    echo "finish-ran rc=$rc"
    exit "$rc"
}
trap _finish EXIT
. "$REPO/infra/forge/cluster-deploy-lib.sh"
tenant_check_alert "fixture tenant refusal" "a detail" 2>> "$CELL/alert.err"
echo "waiting=$(ls "$ALERT_SPOOL" 2>/dev/null | wc -l)"
cat "$CELL/alert.err" | grep -v '^ALERT:\|^alert:' >&2 || true
exit 3
"#;

#[test]
fn the_converge_alerts_and_answers_from_its_exit_trap_with_or_without_a_token() {
    let bench = Bench::new("cluster-deploy-lib");
    let driver = bench.dir.join("deploy.sh");
    write_exec(&driver, DEPLOY);
    judge(
        &bench,
        &Site {
            name: "cluster-deploy-lib.sh tenant_check_alert + answer_converge_requests",
            shell: "bash",
            // One driver for both shapes: it already holds the two
            // contexts the real runner uses — a `( … )` subshell the lib
            // opens itself, and the EXIT trap.
            top: &driver,
            sub: &driver,
            env: &[("STUB_REPLY", "201")],
            sends: &[
                "/api/people?role=platform-admin",
                "-X POST",
                "-X PATCH",
                "/api/jobs/0a1b2c3d-0000-4000-8000-00000000abcd/metadata",
            ],
            // owner read + alert POST (opened by the parent before the
            // subshell) + the PATCH made inside the trap.
            stamped: 3,
            did: &[
                "waiting=0",
                "request 0a1b2c3d annotated converge_failed",
                "finish-ran rc=3",
            ],
        },
    );
    // The run's own exit status survives the trap in every state.
    for slot in SLOTS {
        let run = bench.run("bash", &driver, slot, &[("STUB_REPLY", "201")]);
        assert_eq!(run.rc, 3, "{slot:?}: {}", run.stderr);
    }
}
