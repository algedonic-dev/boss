//! The forge watches its own units, and says so without the thing it
//! watches (backlog c98dcf38).
//!
//! MEASURED 2026-09-26 (triage run ee8412f7, origin/main 0139c378). The
//! unit observer, `infra/estate/observe-units.sh`, ran on boss-gcp
//! alone: roles.toml's `[always]` set is read by boss-gcp's installer,
//! and the forge converges through `infra/forge/install.sh`, which never
//! listed it. So forge-converge.service closed failed 72 times in twelve
//! hours (06:20Z-18:10Z, `Result=exit-code`) and no ESTATE ALARM fired,
//! while the same observer raised `unit_unhealthy:boss-gcp/
//! boss-gcp-converge.service` after three comparisons. The estate chain
//! (compare -> estate.alarm) is host-agnostic; what the forge lacked was
//! an observation to compare.
//!
//! WHAT THIS PINS, by running the REAL observer the way
//! `infra/forge/estate-observe-units.service` runs it — the forge's
//! installer as the roster, `HOST_ID=forge` — against stub `systemctl`,
//! `journalctl` and `curl` on PATH:
//!   * a failed forge-converge.service is in the observation POSTed to
//!     the estate door, `healthy: false`, with its journal lines, on the
//!     node `forge` — the row estate compare turns into
//!     `unit_unhealthy:forge/forge-converge.service`;
//!   * the local half needs no patient (CLAUDE.md §Diagnosis): with the
//!     jobs API dark — the roles read AND the POST refused — the observer
//!     still derives the forge's roster, still names forge-converge.service
//!     as unhealthy in its own output, and exits non-zero, so the forge's
//!     systemd shows a failed observer;
//!   * the control: the same forge with every unit healthy posts a clean
//!     reading and exits 0, so the red above is the unit's state and not
//!     the fixture's.
//!
//! The roster itself (every forge installer row, both halves, the
//! self-exclusion) is pinned by
//! `infra/lint/the-host-observer-watches-what-is-installed.sh` check 9.

use boss_testing::{repo_root, scratch_dir, write_exec};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `systemctl show <unit> --property=...`: every unit loaded and at
/// rest; forge-converge.service failed the way the 72 runs did, unless
/// `STUB_CONVERGE=ok`. The three long-running units the forge installs
/// beside its timer pairs — the ops-runner waker and the two network-boot
/// daemons — run (Type=simple), unless `STUB_WAKE=dead`, which leaves the
/// waker failed the way a refusal (exit 78) leaves it. `systemctl
/// list-units` answers what the forge's own reading of 2026-10-20 found
/// running under the boss- prefix: the runner's timer and its waker.
const SYSTEMCTL: &str = r#"#!/bin/sh
if [ "$1" = "list-units" ]; then
    echo "boss-ops-runner.timer loaded active waiting BOSS ops: answer ops-request packets"
    [ "${STUB_WAKE:-running}" = "running" ] &&
        echo "boss-ops-runner-wake.service loaded active running BOSS ops: wake the ops-request runner"
    exit 0
fi
unit="$2"
case "$unit" in
    *.timer)
        printf 'LoadState=loaded\nActiveState=active\nSubState=waiting\nResult=success\n' ;;
    boss-ops-runner-wake.service)
        if [ "${STUB_WAKE:-running}" = "dead" ]; then
            printf 'LoadState=loaded\nActiveState=failed\nSubState=failed\nResult=exit-code\nExecMainStatus=78\nType=simple\n'
        else
            printf 'LoadState=loaded\nActiveState=active\nSubState=running\nResult=success\nExecMainStatus=0\nType=simple\n'
        fi ;;
    forge-netboot-dhcp.service|forge-netboot-http.service)
        printf 'LoadState=loaded\nActiveState=active\nSubState=running\nResult=success\nExecMainStatus=0\nType=simple\n' ;;
    forge-converge.service)
        if [ "${STUB_CONVERGE:-failed}" = "ok" ]; then
            printf 'LoadState=loaded\nActiveState=inactive\nSubState=dead\nResult=success\nExecMainStatus=0\nType=oneshot\n'
        else
            printf 'LoadState=loaded\nActiveState=failed\nSubState=failed\nResult=exit-code\nExecMainStatus=1\nType=oneshot\n'
        fi ;;
    *)
        printf 'LoadState=loaded\nActiveState=inactive\nSubState=dead\nResult=success\nExecMainStatus=0\nType=oneshot\n' ;;
esac
"#;

const JOURNALCTL: &str = r#"#!/bin/sh
echo "forge-converge.sh: deposit_secret unreadable: /etc/boss-ops/kubeconfig is absent"
echo "systemd[1]: forge-converge.service: Main process exited, code=exited, status=1/FAILURE"
"#;

/// The estate door and the roles read. `STUB_API=dark` refuses both the
/// way an unreachable host does (curl exit 7); otherwise the POST body
/// is kept in `$STUB_DIR/posted.json` and answered 202.
const CURL: &str = r#"#!/bin/sh
[ "${STUB_API:-up}" = "dark" ] && { echo "curl: (7) Failed to connect" >&2; exit 7; }
for a in "$@"; do
    case "$a" in
        */api/estate/observation)
            cat > "$STUB_DIR/posted.json"
            printf '{"accepted":true}\n202'
            exit 0 ;;
        */api/estate/nodes)
            printf '{"data":[{"id":"forge","roles":["cluster-operator","ops-runner"]}]}'
            exit 0 ;;
    esac
done
echo "curl stub: unexpected call: $*" >&2
exit 2
"#;

struct Forge {
    dir: PathBuf,
}

struct Run {
    rc: i32,
    text: String,
}

impl Forge {
    fn new(tag: &str) -> Forge {
        let dir = scratch_dir(&format!("forge-observes-units-{tag}"));
        let bin = dir.join("bin");
        boss_testing::create_dir(&bin);
        write_exec(&bin.join("systemctl"), SYSTEMCTL);
        write_exec(&bin.join("journalctl"), JOURNALCTL);
        write_exec(&bin.join("curl"), CURL);
        Forge { dir }
    }

    /// Run the observer as the forge's unit does. `roles`: the preset
    /// BOSS_NODE_ROLES, or None to read them off the (stub) jobs API.
    fn observe(&self, roles: Option<&str>, env: &[(&str, &str)]) -> Run {
        let root = repo_root();
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new("sh");
        cmd.arg(root.join("infra/estate/observe-units.sh"))
            .env("PATH", path)
            .env("HOST_ID", "forge")
            .env("JOBS_API", "http://jobs.test:7900")
            .env(
                "OBSERVE_UNITS_INSTALLER",
                root.join("infra/forge/install.sh"),
            )
            .env("BOSS_NODE_ROLES_CACHE", self.dir.join("no-cache"))
            .env("STUB_DIR", &self.dir)
            .env_remove("UNITS")
            .env_remove("BOSS_NODE_ROLES")
            .env_remove("BOSS_ESTATE_NODES_URL")
            .env_remove("BOSS_REPO_ROOT");
        if let Some(r) = roles {
            cmd.env("BOSS_NODE_ROLES", r);
        }
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

    fn posted(&self) -> Value {
        let p: &Path = &self.dir.join("posted.json");
        let body = std::fs::read_to_string(p)
            .unwrap_or_else(|e| panic!("no observation was POSTed ({}): {e}", p.display()));
        serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("posted body is not JSON: {e}\n{body}"))
    }
}

fn unit<'a>(node: &'a Value, name: &str) -> &'a Value {
    node["units"]
        .as_array()
        .and_then(|us| us.iter().find(|u| u["unit"] == name))
        .unwrap_or_else(|| panic!("{name} is not in the forge's observation: {node}"))
}

#[test]
fn a_failed_forge_converge_reaches_the_estate_door_as_an_unhealthy_unit() {
    let forge = Forge::new("failed");
    let run = forge.observe(Some("cluster-operator,ops-runner"), &[]);
    assert_eq!(
        run.rc, 1,
        "an unhealthy unit must fail the observer locally too:\n{}",
        run.text
    );

    let obs = forge.posted();
    assert_eq!(obs["scope"], "host-units", "{obs}");
    let node = &obs["nodes"][0];
    assert_eq!(
        node["id"], "forge",
        "the node must be the forge's estate id: {obs}"
    );
    assert_eq!(node["healthy"], false, "{obs}");

    let converge = unit(node, "forge-converge.service");
    assert_eq!(converge["healthy"], false, "{converge}");
    assert_eq!(converge["result"], "exit-code", "{converge}");
    assert!(
        converge["journal"]
            .as_str()
            .is_some_and(|j| j.contains("status=1/FAILURE")),
        "the unhealthy unit carries its journal lines on the event: {converge}"
    );

    // Everything else the forge installs is watched and healthy — so the
    // one finding is the converge, and the roster is the installer's.
    for name in [
        "forge-converge.timer",
        "estate-observe-host.service",
        "cluster-watchdog.timer",
        "estate-observe-units.timer",
        "boss-ops-runner.service",
        "boss-ops-runner-wake.service",
        "forge-netboot-dhcp.service",
        "forge-netboot-http.service",
    ] {
        assert_eq!(unit(node, name)["healthy"], true, "{name}: {obs}");
    }
    let unhealthy: Vec<&Value> = node["units"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|u| u["healthy"] != true)
        .collect();
    assert_eq!(unhealthy.len(), 1, "only the converge is red: {obs}");
    assert!(
        !node["units"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["unit"] == "estate-observe-units.service"),
        "the observer must not watch itself — it would latch: {obs}"
    );
}

#[test]
fn with_the_jobs_api_dark_the_forge_still_names_its_red_unit_and_fails_locally() {
    let forge = Forge::new("dark");
    // No preset roles: the roles read goes to the API, which is dark, and
    // there is no cached declaration.
    let run = forge.observe(None, &[("STUB_API", "dark")]);
    assert_ne!(
        run.rc, 0,
        "a dark API must not turn a red forge into a green observer:\n{}",
        run.text
    );
    assert!(
        run.text
            .lines()
            .any(|l| l.starts_with("observing forge units")
                && l.ends_with("unhealthy: forge-converge.service")),
        "the local half must name the red unit without the API:\n{}",
        run.text
    );
    assert!(
        !forge.dir.join("posted.json").exists(),
        "the stub API was dark, so nothing can have been posted"
    );
}

#[test]
fn a_healthy_forge_posts_a_clean_reading_and_exits_zero() {
    let forge = Forge::new("healthy");
    let run = forge.observe(
        Some("cluster-operator,ops-runner"),
        &[("STUB_CONVERGE", "ok")],
    );
    assert_eq!(run.rc, 0, "{}", run.text);
    let obs = forge.posted();
    assert_eq!(obs["nodes"][0]["id"], "forge", "{obs}");
    assert_eq!(obs["nodes"][0]["healthy"], true, "{obs}");
    assert_eq!(
        unit(&obs["nodes"][0], "forge-converge.service")["healthy"],
        true,
        "{obs}"
    );
    // The waker the 2026-10-20 reading carried as undeclared is watched,
    // so nothing running under the boss- prefix is left outside the roster.
    assert_eq!(
        obs["nodes"][0]["undeclared_units"],
        serde_json::json!([]),
        "{obs}"
    );
}

/// THE WAKER IS WATCHED WHERE IT RUNS (backlog 2eae38d4). On 2026-10-20
/// the forge's own reading carried `boss-ops-runner-wake.service` under
/// `undeclared_units` — running, and outside what the observer judged —
/// so a dead waker would have shown only as ops-request `queued_s`
/// drifting from ~10 s back toward ~33 s while the timer kept every
/// request moving. Dead, it is a red unit on the estate record, the row
/// estate compare turns into `unit_unhealthy:forge/boss-ops-runner-wake.service`,
/// and the observer fails locally too.
#[test]
fn a_dead_waker_on_the_forge_is_a_red_unit_on_the_estate_record() {
    let forge = Forge::new("dead-waker");
    let run = forge.observe(
        Some("cluster-operator,ops-runner"),
        &[("STUB_CONVERGE", "ok"), ("STUB_WAKE", "dead")],
    );
    assert_eq!(
        run.rc, 1,
        "a dead waker must fail the observer locally:\n{}",
        run.text
    );
    let obs = forge.posted();
    let node = &obs["nodes"][0];
    assert_eq!(node["healthy"], false, "{obs}");
    let wake = unit(node, "boss-ops-runner-wake.service");
    assert_eq!(wake["healthy"], false, "{wake}");
    assert_eq!(wake["exec_main_status"], 78, "{wake}");
    let red = node["units"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|u| u["healthy"] != true)
        .count();
    assert_eq!(red, 1, "only the waker is red: {obs}");
}

/// Without the ops-runner role the forge installs no runner, so there is
/// no waker to watch — watching one would read not-found forever, the
/// 2026-09-15 false alarms again. The network-boot daemons stay: the
/// forge installs them whatever its roles.
#[test]
fn a_forge_outside_the_ops_runner_role_watches_no_waker() {
    let forge = Forge::new("no-runner");
    let run = forge.observe(Some("cluster-operator"), &[("STUB_CONVERGE", "ok")]);
    assert_eq!(run.rc, 0, "{}", run.text);
    let obs = forge.posted();
    let watched: Vec<&str> = obs["nodes"][0]["units"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|u| u["unit"].as_str())
        .collect();
    for gone in [
        "boss-ops-runner.timer",
        "boss-ops-runner.service",
        "boss-ops-runner-wake.service",
    ] {
        assert!(!watched.contains(&gone), "{gone}: {obs}");
    }
    for kept in ["forge-netboot-dhcp.service", "forge-netboot-http.service"] {
        assert!(watched.contains(&kept), "{kept}: {obs}");
    }
}
