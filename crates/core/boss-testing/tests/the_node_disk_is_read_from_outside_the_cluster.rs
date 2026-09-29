//! `infra/estate/observe-nodefs.sh` is RUN, not read — against a stub
//! `sudo` that plays `docker run` of kubectl and talosctl, and a stub
//! `curl` that plays the estate door — so every reading below is one the
//! observer actually reached.
//!
//! WHY IT EXISTS (backlog eeac3d56, 2026-09-29). Each cluster node's free
//! space reached the record through the in-cluster observer reading the
//! kubelet's /stats/summary via the API server, authorized as `get
//! nodes/proxy` — and the kubelet authorizes a WebSocket exec opened as a
//! GET as that same grant, so the observer's token could exec into any
//! pod. The reading moved to the forge, which already holds the admin
//! talosconfig, and reads each node's /var through the Talos API; the
//! grant went with the call that needed it.
//!
//! What each case pins: the Talos call is the pinned binary and the
//! placed talosconfig, both mounted READ-ONLY with `--mount` (never `-v`,
//! which would create an absent credential as a root-owned directory),
//! aimed at each node's InternalIP, bounded by `timeout` inside the
//! container; the /var row is read by counting columns from the END, so
//! an answer with or without the NODE column reads the same; figures
//! are GiB by the one rounding rule; a node whose call fails, whose
//! answer has no /var, or that lists no address is RECORDED as unread
//! with the reason, never dropped and never guessed; a node list that
//! cannot be read posts nothing and says whose credential it tried.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/estate/observe-nodefs.sh";

/// `kubectl get nodes -o json` as the cluster answers it, eight nodes:
/// w-1 (answers with the NODE column), cp-3 (answers without it), cp-9
/// (its Talos API refuses), cp-7 (answers, but no /var row), cp-6
/// (lists no InternalIP), w-6 (an IPv6 InternalIP, read like any other),
/// and two whose InternalIP is not one address literal — cp-5 a comma
/// list, which `-n` would read as SEVERAL targets, and cp-4 a hostname,
/// which the endpoint's apid would proxy to whatever it resolves to
/// (review of 4327d1a3, finding 2).
const NODES: &str = r#"{"items":[
 {"metadata":{"name":"w-1"},"status":{"addresses":[{"type":"Hostname","address":"w-1"},{"type":"InternalIP","address":"10.20.0.21"}]}},
 {"metadata":{"name":"cp-3"},"status":{"addresses":[{"type":"InternalIP","address":"10.20.0.13"}]}},
 {"metadata":{"name":"cp-9"},"status":{"addresses":[{"type":"InternalIP","address":"10.20.0.19"}]}},
 {"metadata":{"name":"cp-7"},"status":{"addresses":[{"type":"InternalIP","address":"10.20.0.17"}]}},
 {"metadata":{"name":"cp-6"},"status":{"addresses":[{"type":"Hostname","address":"cp-6"}]}},
 {"metadata":{"name":"w-6"},"status":{"addresses":[{"type":"InternalIP","address":"fd00:20::26"}]}},
 {"metadata":{"name":"cp-5"},"status":{"addresses":[{"type":"InternalIP","address":"10.20.0.15,10.20.0.21"}]}},
 {"metadata":{"name":"cp-4"},"status":{"addresses":[{"type":"InternalIP","address":"evil.example.test"}]}}
]}"#;

/// `talosctl mounts` as v1.13.8 renders it (pkg/machinery/formatters
/// RenderMounts): GB = bytes x 1e-9 to two decimals, a NODE column only
/// when the answer carries node metadata. 997.55 GB is 929.04 GiB and
/// 418.76 GB is 390.00 GiB — w-1's real capacity and the lint's figure.
const W1_MOUNTS: &str = "\
NODE   FILESYSTEM       SIZE(GB)   USED(GB)   AVAILABLE(GB)   PERCENT USED   MOUNTED ON
w-1    /dev/loop0       0.07       0.07       0.00            100.00%        /
w-1    /dev/nvme0n1p6   997.55     578.79     418.76          58.02%         /var
w-1    /dev/nvme0n1p5   0.10       0.01       0.09            6.12%          /system/state
";
const CP3_MOUNTS: &str = "\
FILESYSTEM       SIZE(GB)   USED(GB)   AVAILABLE(GB)   PERCENT USED   MOUNTED ON
/dev/loop0       0.07       0.07       0.00            100.00%        /
/dev/sda6        267.93     4.86       263.07          1.81%          /var
";
const CP7_MOUNTS: &str = "\
NODE   FILESYSTEM   SIZE(GB)   USED(GB)   AVAILABLE(GB)   PERCENT USED   MOUNTED ON
cp-7   /dev/loop0   0.07       0.07       0.00            100.00%        /
";

struct World {
    dir: PathBuf,
    bin: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let dir = scratch_dir(tag);
        let bin = dir.join("bin");
        create_dir(&bin);
        write_file(&dir.join("nodes.json"), NODES);
        write_file(&dir.join("mounts-10.20.0.21.txt"), W1_MOUNTS);
        write_file(&dir.join("mounts-10.20.0.13.txt"), CP3_MOUNTS);
        write_file(&dir.join("mounts-10.20.0.17.txt"), CP7_MOUNTS);
        write_file(&dir.join("mounts-fd00:20::26.txt"), CP3_MOUNTS);
        // sudo: plays `docker run <image> kubectl …` and `… /talosctl …`,
        // logging every call, the way the forge's daemon would answer.
        write_exec(
            &bin.join("sudo"),
            "#!/usr/bin/env bash\n\
             printf '%s\\n' \"$*\" >> \"$STUB_DIR/sudo.calls\"\n\
             case \" $* \" in\n\
             \x20 *' kubectl '*)\n\
             \x20   if [ -n \"${STUB_NODES_FAIL:-}\" ]; then echo 'docker: bind source path does not exist: /x/kubeconfig' >&2; exit 125; fi\n\
             \x20   cat \"$STUB_DIR/nodes.json\"; exit 0 ;;\n\
             \x20 *' /talosctl '*)\n\
             \x20   ip=''; prev=''; for a in \"$@\"; do [ \"$prev\" = -n ] && ip=\"$a\"; prev=\"$a\"; done\n\
             \x20   if [ -f \"$STUB_DIR/mounts-$ip.txt\" ]; then cat \"$STUB_DIR/mounts-$ip.txt\"; exit 0; fi\n\
             \x20   echo 'error getting mounts: rpc error: code = Unavailable desc = connection refused' >&2; exit 1 ;;\n\
             esac\n\
             echo \"sudo stub: unexpected: $*\" >&2; exit 99\n",
        );
        World { dir, bin }
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT))
            .args(args)
            .env("PATH", path)
            .env("STUB_DIR", &self.dir)
            .env("BOSS_OPS_DIR", self.dir.join("ops"))
            .env("BOSS_TALOSCTL", self.dir.join("talosctl-pinned"))
            .env("BOSS_TALOS_TIMEOUT_S", "7")
            // ops-credentials.sh spells the image from the registry host
            // /etc/boss/sor.env carries (backlog cf321ffd).
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OBSERVE_WORK", self.dir.join("work"))
            .env_remove("JOBS_API")
            .env_remove("STUB_NODES_FAIL");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().expect("run observe-nodefs.sh")
    }

    fn observe(&self) -> (Value, String) {
        let out = self.run(&["--print"], &[]);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            out.status.success(),
            "observe-nodefs.sh --print failed: {stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let json = stdout
            .lines()
            .find(|l| l.starts_with('{'))
            .unwrap_or_else(|| panic!("no observation printed: {stdout}"));
        (
            serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}: {json}")),
            stdout,
        )
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.join("sudo.calls")).unwrap_or_default()
    }
}

fn node<'a>(obs: &'a Value, id: &str) -> &'a Value {
    obs["nodes"]
        .as_array()
        .and_then(|a| a.iter().find(|n| n["id"] == id))
        .unwrap_or_else(|| panic!("no node {id} in {obs}"))
}

#[test]
fn every_listed_node_is_read_through_talos_and_recorded() {
    let w = World::new("nodefs-read");
    let (obs, stdout) = w.observe();
    assert_eq!(obs["scope"], "talos-nodefs");
    assert_eq!(obs["mount"], "/var");
    assert_eq!(
        obs["nodes"].as_array().map(Vec::len),
        Some(8),
        "every node the cluster lists is on the reading, read or not: {obs}"
    );
    // An IPv6 InternalIP is an address literal like any other.
    assert_eq!(node(&obs, "w-6")["disk_free_gb"], 245, "{obs}");
    // With the NODE column and without it: the same row, read from the end.
    assert_eq!(node(&obs, "w-1")["disk_gb"], 929, "{obs}");
    assert_eq!(node(&obs, "w-1")["disk_free_gb"], 390, "{obs}");
    assert_eq!(node(&obs, "w-1")["address"], "10.20.0.21", "{obs}");
    assert_eq!(node(&obs, "cp-3")["disk_gb"], 250, "{obs}");
    assert_eq!(node(&obs, "cp-3")["disk_free_gb"], 245, "{obs}");
    assert!(
        stdout.contains("w-1 (10.20.0.21): /var disk=929G free=390G"),
        "{stdout}"
    );
}

#[test]
fn a_node_that_cannot_be_read_is_recorded_unread_with_its_reason() {
    let w = World::new("nodefs-unread");
    let (obs, stdout) = w.observe();
    for (id, why) in [
        ("cp-9", "connection refused"),
        ("cp-7", "listed no filesystem mounted on /var"),
        ("cp-6", "no InternalIP"),
        ("cp-5", "not one IPv4 or IPv6 address literal"),
        ("cp-4", "not one IPv4 or IPv6 address literal"),
    ] {
        let n = node(&obs, id);
        assert!(
            n["disk_free_gb"].is_null() && n["disk_gb"].is_null(),
            "{id}: {n}"
        );
        assert!(
            n["unread"].as_str().is_some_and(|u| u.contains(why)),
            "{id}'s reason must say `{why}`: {n}"
        );
        assert!(
            stdout.contains(&format!("no nodefs reading for {id}")),
            "{id} not named on stdout — the forge's journal is readable with the record dark: {stdout}"
        );
    }
}

#[test]
fn the_talos_call_mounts_the_pinned_binary_and_the_placed_talosconfig_read_only() {
    let w = World::new("nodefs-call");
    w.observe();
    let calls = w.calls();
    let talos: Vec<&str> = calls.lines().filter(|l| l.contains("/talosctl")).collect();
    assert_eq!(
        talos.len(),
        5,
        "one Talos call per node with ONE address literal (cp-6 has none; cp-5's comma \
         list and cp-4's hostname never reach talosctl): {calls}"
    );
    assert!(
        !calls.contains("evil.example.test") && !calls.contains("10.20.0.15,"),
        "an InternalIP that is not one address literal reached talosctl: {calls}"
    );
    let ops = w.dir.join("ops").join("talosconfig");
    let bin = w.dir.join("talosctl-pinned");
    for l in &talos {
        // `-n`: the stub records argv after sudo, so this is `sudo -n docker
        // run`, in the forge registry's mirrored image (backlog cf321ffd).
        assert!(l.starts_with("-n docker run --rm --network host "), "{l}");
        assert!(l.contains(" reg.test/david/alpine-k8s:1.33.3 "), "{l}");
        assert!(
            l.contains(&format!(
                "--mount type=bind,src={},dst=/tc,readonly",
                ops.display()
            )),
            "the talosconfig must be the one placed under BOSS_OPS_DIR, mounted read-only: {l}"
        );
        assert!(
            l.contains(&format!(
                "--mount type=bind,src={},dst=/talosctl,readonly",
                bin.display()
            )),
            "the binary must be the pinned one install-cluster-operator.sh installs: {l}"
        );
        assert!(
            !l.contains(" -v "),
            "`-v` creates an absent source as a root-owned directory: {l}"
        );
        assert!(
            l.contains("timeout 7 /talosctl --talosconfig=/tc -n "),
            "bounded INSIDE the container, aimed at the node's InternalIP: {l}"
        );
        assert!(l.ends_with(" mounts"), "{l}");
    }
    assert!(
        calls
            .lines()
            .any(|l| l.contains("kubectl --kubeconfig=/kc get nodes -o json --request-timeout=20s")),
        "the node list comes through ops_kubectl, the admin kubeconfig's one door, and is \
         bounded — a stalled API server must not wedge the forge's host observer unit \
         (review of 4327d1a3, finding 3): {calls}"
    );
}

#[test]
fn a_node_list_that_cannot_be_read_posts_nothing_and_names_the_credential() {
    let w = World::new("nodefs-nolist");
    let posted = w.dir.join("posted.json");
    write_exec(
        &w.bin.join("curl"),
        &format!(
            "#!/bin/sh\ncat > '{}'\nprintf '%s\\n%s' '{{\"recorded\":true}}' 202\n",
            posted.display()
        ),
    );
    let out = w.run(
        &[],
        &[("JOBS_API", "http://stub"), ("STUB_NODES_FAIL", "1")],
    );
    assert!(
        !out.status.success(),
        "a reading of nothing is a failed run"
    );
    assert!(!posted.exists(), "nothing may be posted for no node list");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("kubeconfig") && stderr.contains("bind source path does not exist"),
        "the refusal must name the credential and docker's own words: {stderr}"
    );
}

#[test]
fn the_reading_is_posted_to_the_estate_door() {
    let w = World::new("nodefs-post");
    let posted = w.dir.join("posted.json");
    write_exec(
        &w.bin.join("curl"),
        &format!(
            "#!/bin/sh\ncat > '{}'\nprintf '%s\\n%s' '{{\"recorded\":true}}' \"${{STUB_STATUS:-202}}\"\n",
            posted.display()
        ),
    );
    let out = w.run(&[], &[("JOBS_API", "http://stub")]);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let body: Value =
        serde_json::from_str(&std::fs::read_to_string(&posted).expect("posted")).expect("json");
    assert_eq!(body["scope"], "talos-nodefs");
    assert_eq!(node(&body, "w-1")["disk_free_gb"], 390);

    // A refused POST is a failed run, and NOT retained: the only reader
    // wants the newest reading, so a replayed old one is worth nothing.
    let out = w.run(&[], &[("JOBS_API", "http://stub"), ("STUB_STATUS", "503")]);
    assert!(
        !out.status.success(),
        "a reading not recorded is a failed run"
    );
}

#[test]
fn posting_needs_the_system_of_record_named() {
    let w = World::new("nodefs-noapi");
    let out = w.run(&[], &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("JOBS_API"));
}

#[test]
fn the_forge_runs_it_after_its_own_host_reading_and_best_effort() {
    let unit = std::fs::read_to_string(repo_root().join("infra/forge/estate-observe-host.service"))
        .expect("the forge's host observer unit");
    let starts: Vec<&str> = unit
        .lines()
        .filter(|l| l.starts_with("ExecStart="))
        .collect();
    assert_eq!(
        starts,
        vec![
            "ExecStart=/home/david/boss/infra/estate/observe-host.sh",
            "ExecStart=-/home/david/boss/infra/estate/observe-nodefs.sh",
        ],
        "the forge's own reading first (the boarding check reads it), then the cluster's \
         nodefs, best-effort, so a Talos failure never fails the host observation"
    );
    // A oneshot's start timeout is infinity by default: a stalled API
    // server or docker daemon would leave the unit activating forever,
    // the timer could never start it again, and the forge's OWN host
    // reading would stop with it (review of 4327d1a3, finding 3).
    let bound = unit
        .lines()
        .find_map(|l| l.strip_prefix("TimeoutStartSec="))
        .unwrap_or_else(|| panic!("the unit declares no TimeoutStartSec — a hang wedges it"));
    assert!(
        !bound.is_empty() && bound != "infinity" && bound != "0",
        "TimeoutStartSec={bound} does not bound the unit"
    );
}
