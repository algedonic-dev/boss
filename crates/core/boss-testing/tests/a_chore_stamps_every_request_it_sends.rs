//! A chore that holds the estate machine token presents it on EVERY
//! request its wrapper sends — and a chore that holds none does all of
//! its work anyway (backlog 37742794, 2026-10-06; part of urgent 2710c8fc).
//!
//! MEASURED 2026-10-06 17:41Z, `GET /api/events/gate-window?gate=machine-gate&hours=72`
//! on origin/main 145fb3a5: 1,415 `machine_gate.would_refuse` facts named
//! `automation:maintenance-timer`, the largest group in the window. Split
//! by route they were not one defect:
//!
//!   GET  /api/jobs   1,327 facts from 259 peers
//!   POST /api/jobs      88 facts from   8 peers
//!
//! and `automation:boss-step` — the other half of the same chores — had
//! 6 in-cluster facts a route. Ten CronJobs mount the
//! `boss-machine-token` Secret, and every one of them was in the 259:
//! `infra/boss-maintenance-wrap.sh` made the header file and handed it to
//! its POST, and sent its open-packet GET with `x-boss-user` alone. So
//! the packet was read as a missing Secret mount ("maintenance chores
//! without the mount") when it was one line of one shared script, and a
//! mount added to every chore would have cleared none of the 1,327.
//!
//! WHAT THIS PINS, through the real three scripts (`boss-chore.sh`, the
//! wrap, `boss-step.sh`) and the real reader `infra/lib/secret-header.sh`,
//! against a stub `boss-api-curl.sh` that writes down each request it is
//! handed and whether a machine-token header FILE came with it:
//!   * with a token mounted and the jobs host in the list, every request
//!     of a run — the wrap's read and its create, the step's read, merge
//!     and flip — carries the header, in a 0600 file and never in argv;
//!   * launched from a subshell or a command substitution, the two ways a
//!     manifest's `bash -c` text can reach the wrapper, the same;
//!   * THE TOKEN IS NEVER REQUIRED: no mount, an empty mount, a blank
//!     slot, a mount that is a file, a slot holding a line break, a slot
//!     nobody may read, and a host that is not the estate's each leave the
//!     run exactly as it was before any token existed — the same five
//!     requests, none stamped, the check run, the packet closed, exit 0.
//!     A chore that fails because of the token is an outage.
//!
//! The fixture token is a made-up string that names itself; no log or
//! assertion message here prints a slot's content, only whether a header
//! file came and what mode it had.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

const KIND: &str = "maintenance-audit-integrity";
const FIXTURE_TOKEN: &str = "fixture-not-a-real-token";

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The jobs API of `a_loop_packet_names_its_host.rs`, plus a request log:
/// one line per call, `<METHOD> <path without its query> <stamp>`, where
/// stamp is `stamped` (an `-H @file` whose one line is the fixture
/// token's header and whose mode is 600), `argv` (the header rode the
/// command line), `wrong` (a file that is not that header, or not 600)
/// or `bare`.
const STUB_API: &str = r#"#!/usr/bin/env bash
set -euo pipefail
method=GET; data=""; url=""; stamp=bare
while [ $# -gt 0 ]; do
    case "$1" in
        -X) method="$2"; shift 2 ;;
        -H)
            case "$2" in
                @*)
                    f="${2#@}"
                    if [ "$(cat "$f")" = "x-boss-machine-token: $STUB_EXPECT_TOKEN" ] \
                        && [ "$(stat -c %a "$f")" = 600 ]; then
                        stamp=stamped
                    else
                        stamp=wrong
                    fi
                    ;;
                [Xx]-[Bb]oss-[Mm]achine-[Tt]oken*) stamp=argv ;;
            esac
            shift 2
            ;;
        -d) data="$2"; shift 2 ;;
        --data-binary) data=$(cat "${2#@}"); shift 2 ;;
        --max-time) shift 2 ;;
        http://*|https://*) url="$1"; shift ;;
        *) shift ;;
    esac
done
store="$STUB_STORE"
[ -s "$store" ] || echo '[]' > "$store"
path="/${url#*://*/}"
echo "$method ${path%%\?*} $stamp" >> "$STUB_LOG"
case "$method" in
    GET)
        kind=$(printf '%s' "$path" | sed -n 's/.*[?&]kind=\([^&]*\).*/\1/p')
        jq -c --arg k "$kind" \
            '[.[] | select(.kind == $k and .status == "open")] | {data: ., total: length}' "$store"
        ;;
    POST)
        id="job-$(jq length "$store")"
        jq -c --arg id "$id" --argjson body "$data" \
            '. + [$body + {id: $id, steps: [{id: ("step-" + $id), spec_slug: "run", title: "run", status: "active", metadata: {}}]}]' \
            "$store" > "$store.new"
        mv "$store.new" "$store"
        jq -c --arg id "$id" '.[] | select(.id == $id)' "$store"
        ;;
    PATCH)
        jid=$(printf '%s' "$path" | sed -n 's#^/api/jobs/\([^/]*\)/steps/.*#\1#p')
        jq -c --arg j "$jid" --argjson p "$data" \
            'map(if .id == $j then .steps[0].metadata = ((.steps[0].metadata // {}) + $p) else . end)' \
            "$store" > "$store.new"
        mv "$store.new" "$store"
        ;;
    PUT)
        jid=$(printf '%s' "$path" | sed -n 's#^/api/jobs/\([^/]*\)/steps/.*#\1#p')
        jq -c --arg j "$jid" --argjson p "$data" \
            'map(if .id == $j then .steps[0] += $p | .status = "closed" else . end)' \
            "$store" > "$store.new"
        mv "$store.new" "$store"
        echo '{}'
        ;;
esac
"#;

/// How the wrapper is reached. A CronJob's `bash -c` text calls it at top
/// level; the other two are the shapes that text can take.
#[derive(Clone, Copy, Debug)]
enum Launch {
    TopLevel,
    Subshell,
    CommandSubstitution,
}

struct Chore {
    bin: PathBuf,
    store: PathBuf,
    log: PathBuf,
    ran: PathBuf,
}

struct Out {
    rc: i32,
    text: String,
    requests: Vec<String>,
    check_ran: bool,
    closed: bool,
}

impl Chore {
    fn new(tag: &str) -> Chore {
        let bin = scratch_dir(&format!("chore-stamps-{tag}"));
        for (name, src) in [
            ("boss-chore.sh", "infra/boss-chore.sh"),
            ("boss-maintenance-wrap.sh", "infra/boss-maintenance-wrap.sh"),
            ("boss-step.sh", "infra/boss-step.sh"),
        ] {
            write_exec(&bin.join(name), &read(src));
        }
        write_exec(&bin.join("boss-api-curl.sh"), STUB_API);
        create_dir(&bin.join("lib"));
        write_file(
            &bin.join("lib/secret-header.sh"),
            &read("infra/lib/secret-header.sh"),
        );
        let store = bin.join("store.json");
        std::fs::write(&store, "[]").unwrap();
        Chore {
            log: bin.join("requests.log"),
            ran: bin.join("check-ran"),
            store,
            bin,
        }
    }

    /// A mount directory whose `current` slot holds `content`.
    fn mount(&self, content: &str) -> PathBuf {
        let dir = self.bin.join("machine-token");
        create_dir(&dir);
        write_file(&dir.join("current"), content);
        dir
    }

    /// One CronJob run: the pod's env, the wrapper, a check that leaves a
    /// mark. `token_dir` is where the Secret is mounted (it may not
    /// exist); `hosts` is the manifest's BOSS_MACHINE_TOKEN_HOSTS.
    fn run(&self, launch: Launch, token_dir: &std::path::Path, hosts: &str) -> Out {
        let _ = std::fs::remove_file(&self.log);
        let _ = std::fs::remove_file(&self.ran);
        std::fs::write(&self.store, "[]").unwrap();
        let chore = format!(
            "'{}' {KIND} 'Audit-log integrity check' -- touch '{}'",
            self.bin.join("boss-chore.sh").display(),
            self.ran.display()
        );
        let text = match launch {
            Launch::TopLevel => format!("set -euo pipefail\n{chore}\n"),
            Launch::Subshell => format!("set -euo pipefail\n( {chore} )\n"),
            Launch::CommandSubstitution => {
                format!("set -euo pipefail\nsaid=$({chore})\nprintf '%s\\n' \"$said\"\n")
            }
        };
        let out = Command::new("bash")
            .arg("-c")
            .arg(&text)
            .env("BOSS_JOBS_URL", "http://jobs.test:7900")
            .env("BOSS_MACHINE_TOKEN_DIR", token_dir)
            .env("BOSS_MACHINE_TOKEN_HOSTS", hosts)
            .env("STUB_STORE", &self.store)
            .env("STUB_LOG", &self.log)
            .env("STUB_EXPECT_TOKEN", FIXTURE_TOKEN)
            .env("KUBERNETES_SERVICE_HOST", "10.96.0.1")
            .env("HOME", &self.bin)
            .env("TMPDIR", &self.bin)
            .env_remove("HOST_ID")
            .env_remove("BOSS_NODE_ID")
            .env_remove("SERVICE_RESULT")
            .env_remove("EXIT_STATUS")
            .env_remove("BOSS_RUN_SUMMARY_FILE")
            .env_remove("BOSS_STEP_DRY_RUN")
            .env_remove("BOSS_STEP_OUTPUT_FILE")
            .env_remove("BOSS_CHORE_FULL_OUTPUT")
            .env_remove("BOSS_MACHINE_TOKEN")
            .env_remove("RUNTIME_DIRECTORY")
            .output()
            .unwrap_or_else(|e| panic!("run the chore: {e}"));
        let store: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&self.store).unwrap()).unwrap();
        Out {
            rc: out.status.code().unwrap_or(-1),
            text: format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
            requests: std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect(),
            check_ran: self.ran.exists(),
            closed: store
                .as_array()
                .is_some_and(|rows| rows.len() == 1 && rows[0]["status"] == "closed"),
        }
    }
}

/// The five requests of one run, in order, each with `stamp`.
fn the_run(stamp: &str) -> Vec<String> {
    [
        "GET /api/jobs",
        "POST /api/jobs",
        "GET /api/jobs",
        "PATCH /api/jobs/job-0/steps/step-job-0/metadata",
        "PUT /api/jobs/job-0/steps/step-job-0",
    ]
    .iter()
    .map(|r| format!("{r} {stamp}"))
    .collect()
}

fn did_its_work(o: &Out, what: &str) {
    assert_eq!(o.rc, 0, "{what}: the chore exited {}:\n{}", o.rc, o.text);
    assert!(o.check_ran, "{what}: the check never ran:\n{}", o.text);
    assert!(
        o.closed,
        "{what}: the packet was not opened and closed:\n{}",
        o.text
    );
}

#[test]
fn a_chore_holding_the_token_stamps_every_request_however_it_is_launched() {
    for launch in [
        Launch::TopLevel,
        Launch::Subshell,
        Launch::CommandSubstitution,
    ] {
        let c = Chore::new(&format!("held-{launch:?}").to_lowercase());
        let dir = c.mount(&format!("{FIXTURE_TOKEN}\n"));
        let o = c.run(launch, &dir, "jobs.test");
        did_its_work(&o, &format!("{launch:?}"));
        assert_eq!(
            o.requests,
            the_run("stamped"),
            "{launch:?}: a chore that mounts the machine token sends every request with it — a \
             request that goes out bare is a would-refuse fact in the machine gate's window, and \
             under enforce it is a chore with no packet. Output:\n{}",
            o.text
        );
    }
}

#[test]
fn a_chore_with_no_usable_token_does_all_of_its_work_unstamped() {
    type Plant = fn(&Chore) -> PathBuf;
    let cases: &[(&str, Plant, &str)] = &[
        ("no-mount", |c| c.bin.join("machine-token"), "jobs.test"),
        (
            "empty-mount",
            |c| {
                let d = c.bin.join("machine-token");
                create_dir(&d);
                d
            },
            "jobs.test",
        ),
        ("blank-slot", |c| c.mount("  \n\n"), "jobs.test"),
        (
            "mount-is-a-file",
            |c| {
                let f = c.bin.join("machine-token");
                write_file(&f, FIXTURE_TOKEN);
                f
            },
            "jobs.test",
        ),
        (
            "line-break-in-slot",
            |c| c.mount(&format!("{FIXTURE_TOKEN}\nx-boss-user: forged\n")),
            "jobs.test",
        ),
        (
            "oversized-slot",
            |c| c.mount(&"a".repeat(5000)),
            "jobs.test",
        ),
        (
            "host-not-the-estates",
            |c| c.mount(FIXTURE_TOKEN),
            ".boss.svc.cluster.local",
        ),
        ("empty-host-list", |c| c.mount(FIXTURE_TOKEN), ""),
    ];
    for (tag, plant, hosts) in cases {
        for launch in [Launch::TopLevel, Launch::Subshell] {
            let c = Chore::new(&format!("{tag}-{launch:?}").to_lowercase());
            let dir = plant(&c);
            let o = c.run(launch, &dir, hosts);
            let what = format!("{tag} / {launch:?}");
            did_its_work(&o, &what);
            assert_eq!(
                o.requests,
                the_run("bare"),
                "{what}: with no usable token the chore sends what it sent before any token \
                 existed. Output:\n{}",
                o.text
            );
            assert!(
                !o.text.contains(FIXTURE_TOKEN),
                "{what}: a slot's content reached the chore's output"
            );
        }
    }
}

/// A slot the chore's uid may not read (a Secret mounted 0400 for another
/// uid). Root reads through any mode, so under root this cannot be
/// planted and the case says so instead of passing on nothing.
#[test]
fn a_slot_the_chore_may_not_read_costs_the_stamp_and_nothing_else() {
    let c = Chore::new("unreadable-slot");
    let dir = c.mount(FIXTURE_TOKEN);
    let slot = dir.join("current");
    std::fs::set_permissions(&slot, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&slot).is_ok() {
        eprintln!(
            "NOT RUN: this uid reads a 0000 file (root), so an unreadable slot cannot be planted"
        );
        return;
    }
    let o = c.run(Launch::TopLevel, &dir, "jobs.test");
    did_its_work(&o, "unreadable slot");
    assert_eq!(o.requests, the_run("bare"), "output:\n{}", o.text);
}

/// The header file is gone when the run is: three scripts each made one,
/// and each one's EXIT trap removed its directory.
#[test]
fn no_header_file_outlives_the_run() {
    let c = Chore::new("cleanup");
    let dir = c.mount(FIXTURE_TOKEN);
    let o = c.run(Launch::TopLevel, &dir, "jobs.test");
    did_its_work(&o, "cleanup");
    let left: Vec<String> = std::fs::read_dir(&c.bin)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("boss-secret-header."))
        .collect();
    assert!(
        left.is_empty(),
        "a machine-token header directory outlived the run under TMPDIR: {left:?}"
    );
}
