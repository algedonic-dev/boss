//! `infra/forge/upgrade-forgejo.sh` — the forge's Forgejo upgraded
//! through a passkey-signed plan (the ops verbs `plan-a-forgejo-upgrade`
//! and `upgrade-forgejo`).
//!
//! Backlog cecf62b3: no runbook and no verb upgraded Longhorn, Kubernetes
//! or Forgejo; each would have been a hand session. The acceptance this
//! file holds the script to: the plan carries the version diff and the
//! health preconditions; each step re-reads the preconditions; a named
//! rollback target exists; and the write runs only the plan whose sha256
//! the passkey signed.
//!
//! RUN against stubs, the way the other forge scripts are tested: `docker`
//! answers compose, inspect, pull, ps and exec out of a state directory
//! (and `compose up` recreates the container on the image the compose
//! file then names), `curl` serves /api/healthz and /api/v1/version from
//! the same state, and the dump is a stub named by BOSS_UPGRADE_BACKUP
//! that writes an archive and `.latest` the way forge-backup.sh does
//! (forge_backup_sh.rs holds forge-backup.sh itself). `awk`, `diff`,
//! `cmp`, `sha256sum` and `jq` are the real ones, because the one-line
//! swap and the hash are the claim.

use std::path::PathBuf;
use std::process::{Command, Output};

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};

const SCRIPT: &str = "infra/forge/upgrade-forgejo.sh";
const FROM_ID: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const TO_ID: &str = "sha256:3333333333333333333333333333333333333333333333333333333333333333";
const MAIN: &str = "0123456789abcdef0123456789abcdef01234567";

const COMPOSE: &str = "services:\n  forgejo:\n    image: codeberg.org/forgejo/forgejo:16.0.2\n    restart: always\n    volumes:\n      - ./data:/data\n    ports:\n      - \"3000:3000\"\n";

const DOCKER: &str = r#"#!/usr/bin/env bash
S=@STATE@
printf '%s\n' "$*" >> "$S/calls"
tag_of() { printf '%s' "${1##*:}"; }
if [ "$1" = compose ]; then
    shift
    [ "$1" = version ] && exit 0
    [ "$1" = -f ] && { f="$2"; shift 2; }
    case "$1" in
        config)
            if [ -e "$S/busy_before_swap" ] && [ "$f" != "$BOSS_FORGE_COMPOSE" ]; then
                cp "$S/busy_before_swap" "$S/busy"
                [ "$(cat "$S/busy")" != hold ] || touch "$BOSS_CONVERGE_HOLD"
            fi
            img=$(sed -n 's/^[[:space:]]*image:[[:space:]]*//p' "$f" | head -n 1)
            printf '{"services":{"forgejo":{"image":"%s"}}}\n' "$img"; exit 0 ;;
        ps) cat "$S/cid"; exit 0 ;;
        up)
            [ "$(cat "$S/up_fails" 2>/dev/null)" = before ] && { echo 'Error response from daemon: boom' >&2; exit 1; }
            img=$(sed -n 's/^[[:space:]]*image:[[:space:]]*//p' "$f" | head -n 1)
            cat "$S/ids/$(tag_of "$img")" > "$S/img"
            echo bbbb000000000000000000000000000000000000000000000000000000000000 > "$S/cid"
            [ -e "$S/stays_old" ] || tag_of "$img" > "$S/served"
            [ "$(cat "$S/up_fails" 2>/dev/null)" = after ] && { echo 'Error after recreation' >&2; exit 1; }
            echo ' Container forgejo  Recreated'; exit 0 ;;
    esac
    exit 9
fi
case "$1" in
    inspect) printf 'true %s\n' "$(cat "$S/img")"; exit 0 ;;
    image)
        ref="${@: -1}"
        [[ "$ref" == sha256:* ]] || [ -e "$S/ids/$(tag_of "$ref")" ] || { echo "No such image: $ref" >&2; exit 1; }
        case "$*" in
            *RepoDigests*) echo "codeberg.org/forgejo/forgejo@sha256:4444444444444444444444444444444444444444444444444444444444444444" ;;
            *) cat "$S/ids/$(tag_of "$ref")" ;;
        esac
        exit 0 ;;
    pull)
        if [ -e "$S/busy_during_pull" ]; then
            cp "$S/busy_during_pull" "$S/busy"
            [ "$(cat "$S/busy")" != hold ] || touch "$BOSS_CONVERGE_HOLD"
        fi
        [ -e "$S/remote/$(tag_of "$2")" ] || { echo "manifest for $2 not found" >&2; exit 1; }
        cp "$S/remote/$(tag_of "$2")" "$S/ids/$(tag_of "$2")"; echo "Status: Downloaded newer image for $2"; exit 0 ;;
    ps) cat "$S/jobs"; exit 0 ;;
    exec)
        case "$*" in
            *rev-parse*) cat "$S/main"; exit 0 ;;
            *merge-base*) exit 0 ;;
        esac ;;
esac
exit 9
"#;

const CURL: &str = r#"#!/usr/bin/env bash
S=@STATE@
out=""; url=""; actor=""; token=""
while [ $# -gt 0 ]; do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        -w|--max-time|--connect-timeout) shift 2 ;;
        -H)
            case "$2" in
                @*) token=$(cat "${2#@}");;
                'x-boss-user: '*) actor="${2#x-boss-user: }";;
            esac
            shift 2 ;;
        -q|-fsS|-sS) shift ;;
        *) url="$1"; shift ;;
    esac
done
case "$url" in
    'http://127.0.0.1:7900/api/jobs?'*)
        [ "$token" = 'x-boss-machine-token: fixture-token' ] || exit 2
        printf '%s' "$actor" | jq -e '.role == "audit-readonly" and .access_tier == "auditor" and .id == "automation:upgrade-forgejo-reader"' >/dev/null || exit 2
        busy=$(cat "$S/busy")
        case "$busy" in
            unreadable) echo 'reader unavailable' >&2; exit 2 ;;
            malformed) echo '{}'; exit 0 ;;
            truncated) echo '{"total":1,"data":[]}'; exit 0 ;;
        esac
        case "$url:$busy" in
            *kind=pr-train*status=open*:train|*kind=gate-run*status=open*:gate) echo '{"total":1,"data":[{"id":"busy"}]}' ;;
            *status=open*|*:no_history) echo '{"total":0,"data":[]}' ;;
            *) echo '{"total":100,"data":[{"id":"historical"}]}' ;;
        esac ;;
    */api/healthz) cat "$S/healthz" > "$out"; printf '%s' "$(cat "$S/healthz_code")" ;;
    */api/v1/version) printf '{"version":"%s+gitea-1.22.0"}' "$(cat "$S/served")" > "$out"; printf 200 ;;
    *) exit 7 ;;
esac
"#;

const BACKUP: &str = r#"#!/usr/bin/env bash
S=@STATE@
echo run >> "$S/backups"
[ -e "$S/backup_fails" ] && { echo 'forge-backup: REFUSED — 3G free' >&2; exit 1; }
[ -e "$S/job_during_backup" ] && echo 'FORGEJO-ACTIONS-TASK-9_WORKFLOW-ci_JOB-test' > "$S/jobs"
[ ! -e "$S/busy_during_backup" ] || cp "$S/busy_during_backup" "$S/busy"
[ "$(cat "$S/busy")" != hold ] || touch "$BOSS_CONVERGE_HOLD"
mkdir -p "$BOSS_FORGE_BACKUP_DIR"
echo archive > "$BOSS_FORGE_BACKUP_DIR/forge-20261022-120000.tar.gz"
echo "$BOSS_FORGE_BACKUP_DIR/forge-20261022-120000.tar.gz" > "$BOSS_FORGE_BACKUP_DIR/.latest"
echo 'forge-backup: OK forge-20261022-120000.tar.gz (4.0K, sha256 0123456789ab) verified: forgejo-db.sql 33 bytes, app.ini, 1 repositories, no registry data'
"#;

const SYSTEMCTL: &str = r#"#!/usr/bin/env bash
S=@STATE@
printf '%s\n' "$*" >> "$S/systemctl-calls"
[ "$*" = 'show -p LoadState -p ActiveState cluster-deploy-runner.service' ] || exit 2
busy=$(cat "$S/busy")
[ "$busy" != systemctl_unreadable ] || exit 1
echo LoadState=loaded
if [ "$busy" = converge ]; then echo ActiveState=activating; else echo ActiveState=inactive; fi
"#;

struct Case {
    dir: PathBuf,
    state: PathBuf,
    compose: PathBuf,
}

fn case(name: &str) -> Case {
    let dir = scratch_dir(&format!("upgrade-forgejo-{name}"));
    let bin = dir.join("bin");
    let state = dir.join("state");
    for d in [&bin, &state, &state.join("ids"), &state.join("remote")] {
        create_dir(d);
    }
    create_dir(&dir.join("forgejo"));
    let compose = dir.join("forgejo/docker-compose.yml");
    write_file(&compose, COMPOSE);
    let s = state.display().to_string();
    write_exec(&bin.join("docker"), &DOCKER.replace("@STATE@", &s));
    write_exec(&bin.join("curl"), &CURL.replace("@STATE@", &s));
    write_exec(&dir.join("backup"), &BACKUP.replace("@STATE@", &s));
    create_dir(&dir.join("machine-token"));
    write_file(&dir.join("machine-token/current"), "fixture-token");
    write_exec(&dir.join("systemctl"), &SYSTEMCTL.replace("@STATE@", &s));
    for (f, body) in [
        (
            "cid",
            "aaaa000000000000000000000000000000000000000000000000000000000000\n",
        ),
        ("img", FROM_ID),
        ("ids/16.0.2", FROM_ID),
        ("remote/16.0.3", TO_ID),
        ("remote/17.0.0", TO_ID),
        ("remote/18.0.0", TO_ID),
        ("served", "16.0.2"),
        ("healthz", "{\"status\":\"pass\"}"),
        ("healthz_code", "200"),
        ("jobs", ""),
        ("main", MAIN),
        ("busy", "idle"),
    ] {
        write_file(&state.join(f), body);
    }
    Case {
        dir,
        state,
        compose,
    }
}

impl Case {
    fn touch(&self, f: &str) {
        write_file(&self.state.join(f), "");
    }
    fn set(&self, f: &str, body: &str) {
        write_file(&self.state.join(f), body);
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new("bash")
            .arg(repo_root().join(SCRIPT))
            .args(args)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.dir.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("BOSS_FORGE_URL", "http://forge.test:3000")
            .env("BOSS_FORGE_COMPOSE", &self.compose)
            .env("BOSS_UPGRADE_BACKUP", self.dir.join("backup"))
            .env("BOSS_JOBS_URL", "http://127.0.0.1:7900")
            .env("BOSS_MACHINE_TOKEN_DIR", self.dir.join("machine-token"))
            .env("BOSS_SYSTEMCTL", self.dir.join("systemctl"))
            .env("BOSS_CONVERGE_HOLD", self.state.join("hold"))
            .env("BOSS_FORGE_BACKUP_DIR", self.dir.join("backups"))
            .env("BOSS_UPGRADE_POLL_S", "0")
            .env("BOSS_UPGRADE_WAIT_S", "2")
            .env_remove("BOSS_FORGE_REPO_PATH")
            .current_dir(&self.dir)
            .output()
            .expect("run upgrade-forgejo.sh")
    }
    fn calls(&self) -> String {
        std::fs::read_to_string(self.state.join("calls")).unwrap_or_default()
    }
    fn compose_now(&self) -> String {
        std::fs::read_to_string(&self.compose).unwrap()
    }
    /// Render the plan for <version>: the plan bytes and their hash.
    fn plan(&self, version: &str) -> (String, String) {
        let o = self.run(&["--plan", version]);
        assert!(o.status.success(), "the plan refused:\n{}", log(&o));
        let err = String::from_utf8_lossy(&o.stderr).into_owned();
        let hash = err
            .lines()
            .find_map(|l| l.strip_prefix("plan-sha256: "))
            .unwrap_or_else(|| panic!("no plan-sha256 on stderr:\n{err}"))
            .to_string();
        (String::from_utf8_lossy(&o.stdout).into_owned(), hash)
    }
}

#[test]
fn pipeline_activity_or_unreadable_record_refuses_before_dump_and_before_swap() {
    for busy in [
        "train",
        "gate",
        "converge",
        "unreadable",
        "malformed",
        "no_history",
        "truncated",
        "systemctl_unreadable",
        "hold",
    ] {
        for when in [
            "before-dump",
            "after-render",
            "during-backup",
            "before-swap",
        ] {
            let c = case(&format!("pipeline-{busy}-{when}"));
            let (_, hash) = c.plan("16.0.3");
            if busy == "hold" {
                // A standing hold, including a dangling symlink, is not idle.
                if when == "before-dump" {
                    std::os::unix::fs::symlink(c.state.join("absent"), c.state.join("hold"))
                        .unwrap();
                } else {
                    c.set(
                        match when {
                            "after-render" => "busy_during_pull",
                            "during-backup" => "busy_during_backup",
                            _ => "busy_before_swap",
                        },
                        busy,
                    );
                }
            } else {
                c.set(
                    match when {
                        "before-dump" => "busy",
                        "after-render" => "busy_during_pull",
                        "during-backup" => "busy_during_backup",
                        _ => "busy_before_swap",
                    },
                    busy,
                );
            }
            let o = c.run(&["16.0.3", &hash]);
            assert_eq!(
                o.status.code(),
                Some(78),
                "{busy}/{when} must refuse:\n{}",
                log(&o)
            );
            assert_eq!(c.compose_now(), COMPOSE, "{busy}/{when} edited compose");
            assert!(
                !c.calls().contains("up -d"),
                "{busy}/{when} recreated Forgejo"
            );
            if when == "before-dump" || when == "after-render" {
                assert!(
                    !c.state.join("backups").exists(),
                    "{busy} took a dump before refusing"
                );
            }
            let err = String::from_utf8_lossy(&o.stderr);
            let boundary = match when {
                "after-render" => "before the dump",
                "during-backup" => "after the dump",
                "before-swap" => "before the swap",
                _ => "at the render",
            };
            assert!(
                err.contains(boundary),
                "{busy}/{when} refused at the wrong boundary: {err}"
            );
        }
    }
}

#[test]
fn the_plan_names_registry_actions_and_clone_source_outage() {
    let c = case("outage");
    let (plan, _) = c.plan("16.0.3");
    for consequence in ["registry", "Actions", "clone source", "600"] {
        assert!(
            plan.contains(consequence),
            "plan omits outage consequence {consequence}: {plan}"
        );
    }
}

fn log(o: &Output) -> String {
    format!(
        "exit {:?}\n--- stdout\n{}--- stderr\n{}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// The verb file's own `effect`, judged by jq as the runner judges it.
fn effect_matches(text: &str) -> bool {
    let spec = Command::new("jq")
        .args(["-er", ".effect"])
        .arg(repo_root().join("infra/ops/verbs/upgrade-forgejo.json"))
        .output()
        .expect("jq reads the verb");
    assert!(spec.status.success(), "upgrade-forgejo declares an effect");
    let re = std::str::from_utf8(&spec.stdout).unwrap().trim();
    text.lines().any(|l| {
        let o = Command::new("jq")
            .args(["-n", "--arg", "re", re, "--arg", "l", l, "$l | test($re)"])
            .output()
            .expect("jq runs");
        assert!(o.status.success(), "jq could not judge {re:?}");
        String::from_utf8_lossy(&o.stdout).trim() == "true"
    })
}

fn has_jq() -> bool {
    Command::new("jq").arg("--version").output().is_ok()
}

/// The plan carries the version diff, the health preconditions and a
/// named rollback target, renders byte-identically twice, and changes
/// nothing: no compose file edit, no dump, no `up`.
#[test]
fn the_plan_carries_the_version_diff_preconditions_and_rollback_target() {
    let c = case("plan");
    let (plan, hash) = c.plan("16.0.3");
    for want in [
        "== version diff ==",
        &format!(
            "from: 16.0.2 — image codeberg.org/forgejo/forgejo:16.0.2 ({FROM_ID}), and /api/v1/version says 16.0.2"
        ),
        &format!("to: 16.0.3 — image codeberg.org/forgejo/forgejo:16.0.3 ({TO_ID})"),
        "the one line 'image: codeberg.org/forgejo/forgejo:16.0.2'",
        "up -d --no-deps --pull never forgejo",
        "== health preconditions (read now, again after the dump, again before the swap) ==",
        "healthz: pass",
        "ci jobs running: 0",
        "keeps 14 archives by default",
        "== rollback target ==",
        &format!("image: codeberg.org/forgejo/forgejo:16.0.2 ({FROM_ID})"),
        "docker-compose.yml.pre-upgrade-16.0.2",
        "a restore of the dump's forgejo-db.sql",
    ] {
        assert!(plan.contains(want), "the plan lacks {want:?}:\n{plan}");
    }
    assert_eq!(hash.len(), 64);
    let (again, hash2) = c.plan("16.0.3");
    assert_eq!(
        (plan, hash),
        (again, hash2),
        "two renders of one state differ"
    );
    assert_eq!(c.compose_now(), COMPOSE, "a plan edited the compose file");
    let calls = c.calls();
    assert!(!calls.contains("up -d"), "a plan ran up:\n{calls}");
    assert!(!c.state.join("backups").exists(), "a plan took a dump");
}

/// The version diff is bounded by the script, never by the packet: not
/// the same release, not an older one, not two majors ahead, and a
/// release the registry does not have renders no plan.
#[test]
fn the_plan_refuses_a_target_that_is_not_one_upgrade_ahead() {
    let c = case("bounds");
    for (target, why, code) in [
        ("16.0.2", "already runs 16.0.2", 78),
        ("16.0.1", "older than the 16.0.2", 78),
        ("18.0.0", "more than one major release", 78),
        ("latest", "a release number X.Y.Z", 78),
        ("16.1.0", "did not finish", 1),
    ] {
        let o = c.run(&["--plan", target]);
        assert_eq!(o.status.code(), Some(code), "{target}:\n{}", log(&o));
        assert!(
            log(&o).contains(why),
            "{target} refused without {why:?}:\n{}",
            log(&o)
        );
    }
    let o = c.run(&["--plan", "17.0.0"]);
    assert!(
        o.status.success(),
        "one major ahead is admitted:\n{}",
        log(&o)
    );
}

/// Each health precondition refuses the plan on its own.
#[test]
fn the_plan_refuses_an_unhealthy_or_busy_forge() {
    let busy = case("busy");
    busy.set("jobs", "FORGEJO-ACTIONS-TASK-7_WORKFLOW-ci_JOB-test\n");
    let o = busy.run(&["--plan", "16.0.3"]);
    assert_eq!(o.status.code(), Some(78), "{}", log(&o));
    assert!(
        log(&o).contains("1 CI job container(s) are running"),
        "{}",
        log(&o)
    );

    let sick = case("sick");
    sick.set("healthz", "{\"status\":\"fail\"}");
    sick.set("healthz_code", "503");
    let o = sick.run(&["--plan", "16.0.3"]);
    assert_eq!(o.status.code(), Some(78), "{}", log(&o));
    assert!(
        log(&o).contains("/api/healthz does not pass"),
        "{}",
        log(&o)
    );

    let liar = case("liar");
    liar.set("served", "16.0.1");
    let o = liar.run(&["--plan", "16.0.3"]);
    assert_eq!(o.status.code(), Some(78), "{}", log(&o));
    assert!(
        log(&o).contains("/api/v1/version says '16.0.1'"),
        "{}",
        log(&o)
    );

    let drift = case("drift");
    drift.set(
        "ids/16.0.2",
        "sha256:9999999999999999999999999999999999999999999999999999999999999999",
    );
    let o = drift.run(&["--plan", "16.0.3"]);
    assert_eq!(o.status.code(), Some(78), "{}", log(&o));
    assert!(log(&o).contains("disagree about what runs"), "{}", log(&o));
}

/// The approved write: dump first, the one line swapped, compose's `up`
/// with nothing pulled, and the effect line the verb declares — naming
/// the rollback target. A second run of the same approval is refused.
#[test]
fn the_approved_write_dumps_swaps_one_line_and_reads_the_upgrade_back() {
    if !has_jq() {
        eprintln!("upgrade_forgejo_sh: SKIPPED — no jq on this box; the gate image has it");
        return;
    }
    let c = case("write");
    let (_, hash) = c.plan("16.0.3");
    let o = c.run(&["16.0.3", &hash]);
    assert!(o.status.success(), "{}", log(&o));
    let out = log(&o);
    assert!(
        effect_matches(&out),
        "no line matches the declared effect:\n{out}"
    );
    assert!(
        out.contains("Rollback target: image codeberg.org/forgejo/forgejo:16.0.2"),
        "{out}"
    );
    assert!(out.contains("database dump"), "{out}");
    assert_eq!(
        c.compose_now(),
        COMPOSE.replace("forgejo:16.0.2", "forgejo:16.0.3"),
        "the compose file is not the old one with exactly the image line changed"
    );
    let saved = c.dir.join("forgejo/docker-compose.yml.pre-upgrade-16.0.2");
    assert_eq!(
        std::fs::read_to_string(&saved).unwrap(),
        COMPOSE,
        "the rollback compose file is not the old one"
    );
    let calls = c.calls();
    assert!(
        calls.contains("up -d --no-deps --pull never forgejo"),
        "{calls}"
    );
    assert!(
        std::fs::read_to_string(c.state.join("backups"))
            .unwrap()
            .lines()
            .count()
            == 1,
        "the write did not take exactly one dump"
    );
    let dump_at = calls
        .find("exec -u git")
        .expect("main was read before the swap");
    let up_at = calls.find("up -d").unwrap();
    assert!(
        dump_at < up_at,
        "main was not read before the swap:\n{calls}"
    );

    // The same approval again: Forgejo now runs 16.0.3, so it refuses.
    let again = c.run(&["16.0.3", &hash]);
    assert_eq!(again.status.code(), Some(78), "{}", log(&again));
    assert!(
        log(&again).contains("already runs 16.0.3"),
        "{}",
        log(&again)
    );
}

/// A hash that is not today's plan changes nothing — no dump, no edit.
#[test]
fn a_write_whose_plan_moved_changes_nothing() {
    let c = case("moved");
    let (_, hash) = c.plan("16.0.3");
    c.set("served", "16.0.2");
    c.set("main", MAIN);
    // The forge restarted onto another container since the signature.
    c.set(
        "cid",
        "cccc000000000000000000000000000000000000000000000000000000000000\n",
    );
    let o = c.run(&["16.0.3", &hash]);
    assert_eq!(o.status.code(), Some(78), "{}", log(&o));
    assert!(log(&o).contains("not the approved plan"), "{}", log(&o));
    assert_eq!(c.compose_now(), COMPOSE);
    assert!(
        !c.state.join("backups").exists(),
        "a moved plan took a dump"
    );
    assert!(!c.calls().contains("up -d"));
}

/// The dump is the rollback target's database half: without it, Forgejo
/// is not touched.
#[test]
fn a_failed_dump_leaves_forgejo_untouched() {
    let c = case("nodump");
    let (_, hash) = c.plan("16.0.3");
    c.touch("backup_fails");
    let o = c.run(&["16.0.3", &hash]);
    assert_eq!(o.status.code(), Some(78), "{}", log(&o));
    assert!(log(&o).contains("the dump did not complete"), "{}", log(&o));
    assert_eq!(c.compose_now(), COMPOSE);
    assert!(!c.calls().contains("up -d"));
}

/// Each step re-reads the preconditions: a CI job that starts while the
/// dump runs stops the run before the swap.
#[test]
fn a_precondition_that_breaks_after_the_dump_stops_the_swap() {
    let c = case("reread");
    let (_, hash) = c.plan("16.0.3");
    c.touch("job_during_backup");
    let o = c.run(&["16.0.3", &hash]);
    assert_eq!(o.status.code(), Some(78), "{}", log(&o));
    assert!(log(&o).contains("(after the dump)"), "{}", log(&o));
    assert_eq!(c.compose_now(), COMPOSE);
    assert!(!c.calls().contains("up -d"));
}

/// A recreated container that never says the target is NOT proven: exit
/// 1, the reads named, the rollback target named, and no effect line.
#[test]
fn an_upgrade_the_read_back_cannot_prove_fails_naming_the_rollback_target() {
    if !has_jq() {
        eprintln!("upgrade_forgejo_sh: SKIPPED — no jq on this box; the gate image has it");
        return;
    }
    let c = case("unproven");
    let (_, hash) = c.plan("16.0.3");
    c.touch("stays_old");
    let o = c.run(&["16.0.3", &hash]);
    assert_eq!(o.status.code(), Some(1), "{}", log(&o));
    let out = log(&o);
    assert!(out.contains("it is NOT proven"), "{out}");
    assert!(out.contains("/api/v1/version 16.0.2"), "{out}");
    assert!(
        out.contains("Rollback target: image codeberg.org/forgejo/forgejo:16.0.2"),
        "{out}"
    );
    assert!(
        !effect_matches(&out),
        "an unproven run printed the effect:\n{out}"
    );
}

#[test]
fn failed_compose_up_restores_the_file_only_if_the_original_container_remains() {
    for recreated in [false, true] {
        let c = case(&format!("compose-up-failure-{recreated}"));
        let (_, hash) = c.plan("16.0.3");
        c.set("up_fails", if recreated { "after" } else { "before" });
        let o = c.run(&["16.0.3", &hash]);
        assert_eq!(o.status.code(), Some(1), "{}", log(&o));
        assert!(!effect_matches(&log(&o)));
        if recreated {
            assert_eq!(c.compose_now(), COMPOSE.replace("16.0.2", "16.0.3"));
            assert!(log(&o).contains("Rollback target:"));
        } else {
            assert_eq!(c.compose_now(), COMPOSE);
            assert!(log(&o).contains("restored the original compose file"));
            assert_eq!(c.plan("16.0.3").1, hash);
        }
    }
}

#[test]
fn the_swap_preserves_the_compose_mode() {
    use std::os::unix::fs::PermissionsExt;

    let c = case("compose-mode");
    std::fs::set_permissions(&c.compose, std::fs::Permissions::from_mode(0o640)).unwrap();
    let (_, hash) = c.plan("16.0.3");
    let o = c.run(&["16.0.3", &hash]);
    assert!(o.status.success(), "{}", log(&o));
    assert_eq!(
        std::fs::metadata(&c.compose).unwrap().permissions().mode() & 0o777,
        0o640
    );
}
