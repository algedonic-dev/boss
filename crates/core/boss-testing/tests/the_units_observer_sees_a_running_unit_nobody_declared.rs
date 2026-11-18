//! The unit observer sees a running unit nobody declared (backlog
//! 6647ac9a; page audit 2cff1d6e, GAP 14).
//!
//! MEASURED 2026-09-23, re-read 2026-10-01. `infra/estate/observe-units.sh`
//! watches a roster DERIVED from the host's declared roles — which is
//! what stopped it filing false alarms for units a host must not run —
//! and asked nothing else of the host. So the retired `boss-ml-api.service`,
//! stopped by retire-second-stack on 2026-09-15 and running again on
//! boss-gcp from 2026-09-20 (PID 1601343, ops-request 5acce6ec), was
//! outside what the observer watched BY CONSTRUCTION: every five-minute
//! reading said "15 units watched, all healthy" (the boss-gcp reading of
//! 2026-10-01T11:49:01Z still names those 15 and nothing else), and the
//! estate record and /it/estate read the host clean.
//!
//! WHAT THIS PINS, by running the REAL observer the way
//! `infra/estate/boss-estate-observe-units.service` runs it on boss-gcp —
//! the default installer, `HOST_ID=boss-gcp`, the host's live roles —
//! against stub `systemctl`, `journalctl` and `curl` on PATH:
//!   * a running `boss-*` unit outside the roster rides the observation
//!     as `undeclared_units`, with its states, while the roster's own
//!     running units and the observer's own service do not;
//!   * it is paperwork, not a sick unit: the node stays healthy and the
//!     observer exits 0 (estate.alarm does not raise on
//!     observed_not_declared, and this must not latch the observer red);
//!   * an enumeration that fails is recorded as `undeclared_unread` with
//!     its reason — never as an empty list, which would read clean;
//!   * a hand run (`UNITS=`) watches a list nobody declared, so it makes
//!     no claim about undeclared units at all.

use boss_testing::{repo_root, scratch_dir, write_exec};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `systemctl show` answers every unit loaded and at rest (timers armed);
/// `systemctl list-units` answers what boss-gcp was running on
/// 2026-09-23: two roster units, the observer's own oneshot mid-run, and
/// the retired ML API — unless `STUB_LIST=clean` (the roster only) or
/// `STUB_LIST=fail` (systemctl refuses).
const SYSTEMCTL: &str = r#"#!/bin/sh
if [ "$1" = "list-units" ]; then
    printf '%s\n' "$@" > "$STUB_DIR/list-args"
    case "${STUB_LIST:-ml-api}" in
    fail) echo "Failed to connect to bus: No such file or directory" >&2; exit 1 ;;
    forge)
        echo "forge-converge.timer loaded active waiting converge"
        echo "forge-netboot-http.service loaded active running netboot"
        echo "estate-observe-units.service loaded activating start observer"
        echo "forge-unplanned.service loaded active running unplanned"
        echo "cluster-unplanned.timer loaded activating start unplanned"
        echo "boss-ops-runner-wake.service loaded active running retired-runner"
        echo "ssh.service loaded active running unrelated"
        exit 0 ;;
    orchard)
        echo "orchard-unplanned.service loaded active running unplanned"
        echo "forge-unrelated.service loaded active running unrelated"
        exit 0 ;;
    esac
    echo "boss-gcp-converge.timer loaded active waiting BOSS: converge this host"
    echo "boss-estate-observe-units.service loaded activating start BOSS: observe this host's units"
    echo "boss-ops-runner.timer loaded active waiting BOSS: answer ops-request packets"
    [ "${STUB_LIST:-ml-api}" = "ml-api" ] &&
        echo "boss-ml-api.service loaded active running BOSS ML API"
    exit 0
fi
case "$2" in
    *.timer)
        printf 'LoadState=loaded\nActiveState=active\nSubState=waiting\nResult=success\n' ;;
    *)
        printf 'LoadState=loaded\nActiveState=inactive\nSubState=dead\nResult=success\nExecMainStatus=0\nType=oneshot\n' ;;
esac
"#;

const JOURNALCTL: &str = "#!/bin/sh\nexit 0\n";

/// The estate door: the POST body is kept in `$STUB_DIR/posted.json`
/// and answered 202.
const CURL: &str = r#"#!/bin/sh
for a in "$@"; do
    case "$a" in
        */api/estate/observation)
            cat > "$STUB_DIR/posted.json"
            printf '{"accepted":true}\n202'
            exit 0 ;;
    esac
done
echo "curl stub: unexpected call: $*" >&2
exit 2
"#;

/// boss-gcp's roles as the registry declared them on 2026-10-01.
const ROLES: &str =
    "cluster-operator,ml-batch-host,off-cluster-observer,ops-runner,wireguard-bastion";

struct Host {
    dir: PathBuf,
}

struct Run {
    rc: i32,
    text: String,
}

impl Host {
    fn new(tag: &str) -> Host {
        let dir = scratch_dir(&format!("units-observer-undeclared-{tag}"));
        let bin = dir.join("bin");
        boss_testing::create_dir(&bin);
        write_exec(&bin.join("systemctl"), SYSTEMCTL);
        write_exec(&bin.join("journalctl"), JOURNALCTL);
        write_exec(&bin.join("curl"), CURL);
        Host { dir }
    }

    fn observe(&self, env: &[(&str, &str)]) -> Run {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new("sh");
        cmd.arg(repo_root().join("infra/estate/observe-units.sh"))
            .current_dir(&self.dir)
            .env("PATH", path)
            .env("HOST_ID", "boss-gcp")
            .env("JOBS_API", "http://jobs.test:7900")
            .env("BOSS_NODE_ROLES", ROLES)
            .env("STUB_DIR", &self.dir)
            .env_remove("UNITS")
            .env_remove("OBSERVE_UNITS_INSTALLER")
            .env_remove("BOSS_REPO_ROOT");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run observe-units.sh");
        Run {
            rc: out.status.code().unwrap_or(-1),
            text: format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        }
    }

    fn node(&self) -> Value {
        let p: &Path = &self.dir.join("posted.json");
        let body = std::fs::read_to_string(p)
            .unwrap_or_else(|e| panic!("no observation was POSTed ({}): {e}", p.display()));
        let obs: Value = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("posted body is not JSON: {e}\n{body}"));
        obs["nodes"][0].clone()
    }
}

#[test]
fn a_running_unit_the_roster_does_not_declare_rides_the_observation() {
    let host = Host::new("ml-api");
    let run = host.observe(&[]);
    assert_eq!(
        run.rc, 0,
        "an undeclared unit is paperwork, not a sick watched unit — the observer must not fail:\n{}",
        run.text
    );
    let node = host.node();
    assert_eq!(node["id"], "boss-gcp", "{node}");
    assert_eq!(
        node["undeclared_units"],
        json!([{ "unit": "boss-ml-api.service", "active_state": "active", "sub_state": "running" }]),
        "the retired ML API is running and nobody declared it; the roster's own running units and the observer itself are not undeclared: {node}"
    );
    assert_eq!(
        node["healthy"], true,
        "the watched units are all healthy: {node}"
    );
    assert!(node.get("undeclared_unread").is_none(), "{node}");
    assert!(
        run.text
            .lines()
            .any(|l| l.starts_with("observing boss-gcp units")
                && l.contains("undeclared: boss-ml-api.service")),
        "the local half names it too:\n{}",
        run.text
    );
}

#[test]
fn the_forge_scans_its_installer_families_instead_of_only_boss_units() {
    let host = Host::new("forge-families");
    std::fs::write(host.dir.join("forge-local-file"), "not a unit").unwrap();
    let installer = repo_root().join("infra/forge/install.sh");
    let run = host.observe(&[
        ("HOST_ID", "forge"),
        ("BOSS_NODE_ROLES", "ops-runner"),
        ("OBSERVE_UNITS_INSTALLER", installer.to_str().unwrap()),
        ("STUB_LIST", "forge"),
    ]);
    assert_eq!(run.rc, 0, "{}", run.text);
    let node = host.node();
    assert_eq!(
        node["undeclared_units"],
        json!([
            {"unit":"forge-unplanned.service", "active_state":"active", "sub_state":"running"},
            {"unit":"cluster-unplanned.timer", "active_state":"activating", "sub_state":"start"}
        ]),
        "installer families include unprefixed forge units; declared, excluded and unrelated units stay out: {node}"
    );
    let args = std::fs::read_to_string(host.dir.join("list-args")).unwrap();
    for pattern in ["boss-*", "forge-*", "cluster-*"] {
        assert_eq!(
            args.lines().filter(|arg| *arg == pattern).count(),
            1,
            "{pattern}: {args}"
        );
    }
    assert_eq!(node["healthy"], true, "{node}");
}

#[test]
fn a_retired_runner_stays_visible_after_the_forge_loses_its_role() {
    let host = Host::new("forge-without-runner-role");
    let installer = repo_root().join("infra/forge/install.sh");
    let run = host.observe(&[
        ("HOST_ID", "forge"),
        ("BOSS_NODE_ROLES", "cluster-operator"),
        ("OBSERVE_UNITS_INSTALLER", installer.to_str().unwrap()),
        ("STUB_LIST", "forge"),
    ]);
    assert_eq!(run.rc, 0, "{}", run.text);
    let node = host.node();
    assert_eq!(
        node["undeclared_units"],
        json!([
            {"unit":"forge-unplanned.service", "active_state":"active", "sub_state":"running"},
            {"unit":"cluster-unplanned.timer", "active_state":"activating", "sub_state":"start"},
            {"unit":"boss-ops-runner-wake.service", "active_state":"active", "sub_state":"running"}
        ]),
        "removing the role must not hide a runner that is still active: {node}"
    );
    assert_eq!(
        node["healthy"], true,
        "undeclared units remain paperwork: {node}"
    );
    let args = std::fs::read_to_string(host.dir.join("list-args")).unwrap();
    assert_eq!(
        args.lines().filter(|arg| *arg == "boss-*").count(),
        1,
        "{args}"
    );
}

#[test]
fn a_new_installer_family_enters_the_scan_without_an_observer_edit() {
    let host = Host::new("new-family");
    let infra = host.dir.join("source/infra");
    let source = infra.join("custom");
    boss_testing::create_dir(&source);
    for extension in ["service", "timer"] {
        std::fs::write(source.join(format!("orchard-watch.{extension}")), "fixture").unwrap();
    }
    let installer = source.join("install.sh");
    write_exec(
        &installer,
        "#!/bin/sh\ncase \"$1\" in\nrows) echo orchard-watch:custom;;\nroster) echo 'in-role orchard-watch';;\nbeyond-rows) :;;\n*) exit 2;;\nesac\n",
    );
    let run = host.observe(&[
        ("OBSERVE_UNITS_INSTALLER", installer.to_str().unwrap()),
        ("STUB_LIST", "orchard"),
    ]);
    assert_eq!(run.rc, 0, "{}", run.text);
    let node = host.node();
    assert_eq!(
        node["undeclared_units"],
        json!([{"unit":"orchard-unplanned.service", "active_state":"active", "sub_state":"running"}]),
        "the custom installer supplies the scope, including filtering another host's family: {node}"
    );
    let args = std::fs::read_to_string(host.dir.join("list-args")).unwrap();
    assert!(args.lines().any(|arg| arg == "orchard-*"), "{args}");
    assert!(!args.lines().any(|arg| arg == "forge-*"), "{args}");
}

#[test]
fn a_host_running_only_its_roster_reports_an_empty_undeclared_list() {
    let host = Host::new("clean");
    let run = host.observe(&[("STUB_LIST", "clean")]);
    assert_eq!(run.rc, 0, "{}", run.text);
    let node = host.node();
    assert_eq!(
        node["undeclared_units"],
        json!([]),
        "looked and found none is an empty list, not an absent one: {node}"
    );
}

#[test]
fn an_enumeration_that_fails_is_recorded_unread_never_as_none() {
    let host = Host::new("fail");
    let run = host.observe(&[("STUB_LIST", "fail")]);
    assert_eq!(
        run.rc, 0,
        "the watched units were read; a failed enumeration is recorded, not fatal:\n{}",
        run.text
    );
    let node = host.node();
    assert!(
        node.get("undeclared_units").is_none(),
        "an unread enumeration must not read as an empty one: {node}"
    );
    assert!(
        node["undeclared_unread"]
            .as_str()
            .is_some_and(|r| r.contains("list-units")),
        "the reading says it could not look, and why: {node}"
    );
}

#[test]
fn a_hand_run_with_its_own_units_makes_no_undeclared_claim() {
    let host = Host::new("hand");
    let run = host.observe(&[("UNITS", "boss-gcp-converge.timer")]);
    assert_eq!(run.rc, 0, "{}", run.text);
    let node = host.node();
    assert!(
        node.get("undeclared_units").is_none() && node.get("undeclared_unread").is_none(),
        "a hand list is not the declaration, so nothing outside it is undeclared: {node}"
    );
}

/// boss-gcp runs an ops-request runner and deliberately NO waker (backlog
/// 2eae38d4): its runner starts through the root tree's launcher, which
/// holds the tree's shared lock for the life of what it starts, and a
/// waker never exits — so `install-ops-runner.sh` installs none there.
/// The observer asks the same predicate, so it watches the runner's pair
/// and not a waker that would read not-found every five minutes.
#[test]
fn boss_gcp_watches_its_runner_and_no_waker_it_never_installs() {
    let host = Host::new("no-waker");
    let run = host.observe(&[("STUB_LIST", "clean")]);
    assert_eq!(run.rc, 0, "{}", run.text);
    let node = host.node();
    let watched: Vec<&str> = node["units"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|u| u["unit"].as_str())
        .collect();
    for runner in ["boss-ops-runner.timer", "boss-ops-runner.service"] {
        assert!(watched.contains(&runner), "{runner}: {node}");
    }
    assert!(
        !watched.contains(&"boss-ops-runner-wake.service"),
        "boss-gcp installs no waker, so it must not watch one: {node}"
    );
}
