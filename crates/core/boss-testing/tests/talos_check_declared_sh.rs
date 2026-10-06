//! `infra/cluster/talos/check-declared.sh` is RUN, not read — against
//! synthetic `talosctl get machineconfig -o yaml` documents built from
//! the values David read off the LIVE nodes on 2026-09-15 (backlog
//! 08430090), so every verdict below is one the comparator actually
//! reached. Nothing here touches a cluster or needs a credential.
//!
//! WHY THIS FILE EXISTS. The gate depends on Talos machine-config entries
//! (w-1's `machine.files` + `machine.kubelet.extraMounts` for the gate
//! seed, the kubelet image-GC thresholds, the registry mirror the whole
//! cluster pulls through) that until this car lived only on a laptop.
//! `infra/cluster/talos/patches/<node>.yaml` is now the declaration and
//! the comparator reads the live config against it in the vocabulary
//! design 16115a17 decided: MATCH / DRIFT (both values printed) /
//! UNDECLARED, plus ABSENT and DOUBLED — the second is the w-1 case as
//! found: the gate-seed `files` entry applied by hand twice on
//! 2026-09-12, identical, so the live config carries it two times.
//!
//! The live document is doubled at a second level too: `talosctl get
//! machineconfig` prints the config resource more than once (two copies
//! in David's output, ~115 lines apart on w-1), so a comparator that
//! counted entries across documents would read every node as DOUBLED.
//! The fixture carries that shape on purpose.

use boss_testing::{feed_stdin, repo_root};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(case: &str) -> PathBuf {
    boss_testing::scratch_dir(&format!("talos-check-declared-{case}"))
}

fn script() -> PathBuf {
    repo_root().join("infra/cluster/talos/check-declared.sh")
}

fn patches_dir() -> PathBuf {
    repo_root().join("infra/cluster/talos/patches")
}

/// What a node's live config carries, in the classes the comparator
/// reads. Defaults are "nothing", so each case says exactly what its
/// live node has.
#[derive(Clone, Default)]
struct Live {
    hostname: Option<&'static str>,
    /// How many copies of the gate-seed `machine.files` entry (w-1 live
    /// on 2026-09-15: 2).
    seed_file_copies: usize,
    seed_mount: bool,
    /// `(imageGCHighThresholdPercent, imageGCLowThresholdPercent)`.
    image_gc: Option<(u32, u32)>,
    mirror: bool,
    /// An extra mount the declaration does not name — the shape of the
    /// `- rw` David's grep showed on every control plane before its
    /// `extraConfig:`.
    other_mount: bool,
    /// The kubelet bind for the gate volume at /var/mnt/gate (backlog
    /// 52ea56ac, declared 2026-09-30 for w-1's second NVMe).
    gate_mount: bool,
    /// The `gate` UserVolumeConfig document, by its disk selector. A
    /// config carrying one is MULTI-document, so the resource prints its
    /// spec as a block string (the only way two documents fit under one
    /// `spec:` key) — the other shape find_configs was written to read.
    gate_volume: Option<&'static str>,
}

/// The selector design 8457c07b decided for the gate disk: never a
/// serial or a kernel name, because adding a disk renumbered the forge's
/// devices on 2026-09-21.
const GATE_SELECTOR: &str = r#"!system_disk && disk.transport == "nvme" && disk.size > 500u * GB"#;

/// The `gate` user volume as a second config document, at column 0.
fn gate_volume_doc(selector: &str) -> String {
    format!(
        "apiVersion: v1alpha1\n\
         kind: UserVolumeConfig\n\
         name: gate\n\
         provisioning:\n\
         \x20   diskSelector:\n\
         \x20       match: '{selector}'\n\
         \x20   minSize: 500GB\n\
         \x20   grow: true\n\
         filesystem:\n\
         \x20   type: xfs\n"
    )
}

/// Indent every non-empty line by `n` spaces.
fn indent(text: &str, n: usize) -> String {
    let pad = " ".repeat(n);
    text.lines()
        .map(|l| {
            if l.is_empty() {
                "\n".to_string()
            } else {
                format!("{pad}{l}\n")
            }
        })
        .collect()
}

/// The `machine:` section of one config, indented as `talosctl` prints
/// it (4 spaces, the whole config nested under `spec:`).
fn machine_section(live: &Live) -> String {
    let mut s = String::new();
    s.push_str("    machine:\n");
    s.push_str("        type: worker\n");
    s.push_str("        token: REDACTED\n");
    s.push_str("        kubelet:\n");
    s.push_str("            image: ghcr.io/siderolabs/kubelet:v1.33.0\n");
    if live.seed_mount || live.other_mount || live.gate_mount {
        s.push_str("            extraMounts:\n");
        if live.other_mount {
            s.push_str(
                "                - destination: /var/lib/longhorn\n\
                 \x20                 type: bind\n\
                 \x20                 source: /var/lib/longhorn\n\
                 \x20                 options:\n\
                 \x20                   - bind\n\
                 \x20                   - rshared\n\
                 \x20                   - rw\n",
            );
        }
        if live.seed_mount {
            s.push_str(
                "                - destination: /var/local/gate-seed\n\
                 \x20                 type: bind\n\
                 \x20                 source: /var/local/gate-seed\n\
                 \x20                 options:\n\
                 \x20                   - bind\n\
                 \x20                   - rshared\n\
                 \x20                   - rw\n",
            );
        }
        if live.gate_mount {
            s.push_str(
                "                - destination: /var/mnt/gate\n\
                 \x20                 type: bind\n\
                 \x20                 source: /var/mnt/gate\n\
                 \x20                 options:\n\
                 \x20                   - bind\n\
                 \x20                   - rshared\n\
                 \x20                   - rw\n",
            );
        }
    }
    if let Some((high, low)) = live.image_gc {
        s.push_str("            extraConfig:\n");
        s.push_str(&format!(
            "                imageGCHighThresholdPercent: {high}\n"
        ));
        s.push_str(&format!(
            "                imageGCLowThresholdPercent: {low}\n"
        ));
    }
    s.push_str("            defaultRuntimeSeccompProfileEnabled: true\n");
    s.push_str("            disableManifestsDirectory: true\n");
    s.push_str("        network:\n");
    if let Some(h) = live.hostname {
        s.push_str(&format!("            hostname: {h}\n"));
    }
    s.push_str("            interfaces:\n");
    s.push_str("                - interface: eth0\n");
    s.push_str("                  dhcp: true\n");
    s.push_str("        install:\n");
    s.push_str("            disk: /dev/nvme0n1\n");
    s.push_str("            wipe: false\n");
    if live.seed_file_copies > 0 {
        s.push_str("        files:\n");
        for _ in 0..live.seed_file_copies {
            s.push_str(
                "            - content: |\n\
                 \x20               local PV gate-seed-w-1 (infra/cluster/manifests/gate-seed-local.yaml) mounts this directory\n\
                 \x20             permissions: 0o644\n\
                 \x20             path: /var/local/gate-seed/.declared-by-talos\n\
                 \x20             op: create\n",
            );
        }
    }
    if live.mirror {
        s.push_str(
            "        registries:\n\
             \x20           mirrors:\n\
             \x20               10.20.0.15:3000:\n\
             \x20                   endpoints:\n\
             \x20                       - http://10.20.0.15:3000\n",
        );
    }
    s.push_str("        features:\n");
    s.push_str("            rbac: true\n");
    s.push_str("            stableHostname: true\n");
    s
}

/// The node's whole machine config as the node stores it — what
/// `talosctl read /system/state/config.yaml` prints: the v1alpha1
/// document at column 0, then any further document after `---`.
fn raw_config(live: &Live) -> String {
    let body: String = config_body(live)
        .lines()
        .map(|l| format!("{}\n", l.strip_prefix("    ").unwrap_or(l)))
        .collect();
    match live.gate_volume {
        Some(sel) => format!("{body}---\n{}", gate_volume_doc(sel)),
        None => body,
    }
}

/// One `MachineConfigs.config.talos.dev` resource as `talosctl get
/// machineconfig -o yaml` prints it, with the `cluster:` tail a real
/// document carries after `machine:`. A single-document config prints
/// its spec as a mapping; a multi-document one (a user volume beside
/// v1alpha1) as a block string holding every document.
fn resource(node_ip: &str, id: &str, live: &Live) -> String {
    let spec = if live.gate_volume.is_some() {
        format!("spec: |\n{}", indent(&raw_config(live), 4))
    } else {
        format!("spec:\n{}", config_body(live))
    };
    format!(
        "node: {node_ip}\n\
         metadata:\n\
         \x20   namespace: config\n\
         \x20   type: MachineConfigs.config.talos.dev\n\
         \x20   id: {id}\n\
         \x20   version: 3\n\
         \x20   owner: config.MachineConfigController\n\
         \x20   phase: running\n\
         {spec}"
    )
}

/// The v1alpha1 document, indented four spaces as it sits under `spec:`.
fn config_body(live: &Live) -> String {
    format!(
        "\x20   version: v1alpha1 # Indicates the schema used to decode the contents.\n\
         \x20   debug: false\n\
         \x20   persist: true\n\
         {machine}\
         \x20   cluster:\n\
         \x20       id: REDACTED\n\
         \x20       secret: REDACTED\n\
         \x20       controlPlane:\n\
         \x20           endpoint: https://10.20.0.10:6443\n\
         \x20       network:\n\
         \x20           dnsDomain: cluster.local\n\
         \x20           podSubnets:\n\
         \x20               - 10.244.0.0/16\n\
         \x20           serviceSubnets:\n\
         \x20               - 10.96.0.0/12\n\
         \x20       token: REDACTED\n\
         \x20       ca:\n\
         \x20           crt: REDACTED\n\
         \x20           key: \"\"\n",
        machine = machine_section(live),
    )
}

/// The whole `talosctl get machineconfig -o yaml` output: the config
/// resource printed TWICE (the shape of David's 2026-09-15 read), the
/// two copies identical, separated the way talosctl separates documents.
fn live_yaml(node_ip: &str, live: &Live) -> String {
    format!(
        "{}---\n{}",
        resource(node_ip, "v1alpha1", live),
        resource(node_ip, "persistent", live)
    )
}

/// w-1 exactly as read on 2026-09-15: the seed file entry twice, the
/// seed mount once, the mirror, another mount the declaration does not
/// name.
fn w1_as_found() -> Live {
    Live {
        hostname: Some("w-1"),
        seed_file_copies: 2,
        seed_mount: true,
        image_gc: None,
        mirror: true,
        other_mount: false,
        gate_mount: false,
        gate_volume: None,
    }
}

/// w-1 as its declaration says it should be once David has applied the
/// gate volume (52ea56ac) and removed the doubled seed file entry. No
/// /var/mnt/gate kubelet mount: review a79746c6 withdrew it.
fn w1_as_declared() -> Live {
    Live {
        seed_file_copies: 1,
        gate_volume: Some(GATE_SELECTOR),
        ..w1_as_found()
    }
}

fn run(args: &[&str], stdin: &str, patches: Option<&Path>, path_env: Option<&Path>) -> Output {
    use std::process::Stdio;
    // `/bin/bash` by absolute path: the no-python case empties PATH, and
    // the shell must still be found for the script to refuse in.
    let mut cmd = Command::new("/bin/bash");
    cmd.arg(script())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(p) = patches {
        cmd.env("BOSS_TALOS_PATCHES", p);
    }
    if let Some(p) = path_env {
        cmd.env("PATH", p);
    }
    let mut child = cmd.spawn().expect("spawn check-declared.sh");
    // A usage refusal exits before it reads stdin, so the write races
    // the exit (backlog 28f29f0b); the one tolerant write lives in
    // boss_testing::feed_stdin, shared with the dns twin (d0eafe94).
    feed_stdin(&mut child, stdin.as_bytes());
    child.wait_with_output().unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

/// The count of lines starting with a given verdict word.
fn lines_with(out: &Output, verdict: &str) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with(verdict))
        .map(str::to_string)
        .collect()
}

#[test]
fn w1_as_found_reads_the_doubled_seed_file_as_doubled_and_exits_1() {
    let out = run(
        &["w-1"],
        &live_yaml("10.20.0.14", &w1_as_found()),
        None,
        None,
    );
    let t = text(&out);
    let doubled = lines_with(&out, "DOUBLED");
    assert_eq!(doubled.len(), 1, "one DOUBLED finding expected:\n{t}");
    assert!(
        doubled[0].contains("machine.files[/var/local/gate-seed/.declared-by-talos]")
            && doubled[0].contains("2 copies"),
        "DOUBLED names the entry and the count:\n{t}"
    );
    // The doubled DOCUMENT (two resources) must not double anything else:
    // the mount and the mirror are declared once and found once.
    assert!(
        t.contains("MATCH      machine.kubelet.extraMounts[/var/local/gate-seed]"),
        "the mount matches:\n{t}"
    );
    assert!(
        t.contains("MATCH      machine.registries.mirrors[10.20.0.15:3000]"),
        "the mirror matches:\n{t}"
    );
    assert_eq!(code(&out), 1, "any DOUBLED is exit 1:\n{t}");
}

#[test]
fn w1_deduped_equals_the_declaration_and_exits_0() {
    let live = w1_as_declared();
    let out = run(&["w-1"], &live_yaml("10.20.0.14", &live), None, None);
    let t = text(&out);
    assert_eq!(
        code(&out),
        0,
        "a live config equal to the declaration is exit 0:\n{t}"
    );
    assert_eq!(
        lines_with(&out, "MATCH").len(),
        4,
        "file, the seed mount, mirror, the gate volume:\n{t}"
    );
    assert!(
        t.contains("MATCH      UserVolumeConfig[gate]"),
        "the user volume is read out of the multi-document spec:\n{t}"
    );
    for v in ["DRIFT", "ABSENT", "DOUBLED", "UNDECLARED"] {
        assert!(lines_with(&out, v).is_empty(), "no {v} expected:\n{t}");
    }
}

/// `talosctl read /system/state/config.yaml` — the config as the node
/// stores it, every document at column 0 — reads exactly like the
/// resource form. It is the form the apply sequence feeds the check,
/// because it is what the node holds rather than a rendering of it.
#[test]
fn the_stored_config_reads_like_the_resource_and_finds_the_gate_volume() {
    let out = run(&["w-1"], &raw_config(&w1_as_declared()), None, None);
    let t = text(&out);
    assert_eq!(code(&out), 0, "{t}");
    assert_eq!(lines_with(&out, "MATCH").len(), 4, "{t}");
    assert!(t.contains("MATCH      UserVolumeConfig[gate]"), "{t}");
    assert!(t.contains("id=(raw)"), "says which document it read:\n{t}");
}

/// Before David applies the patch, the node has no gate volume: it is
/// named ABSENT, and nothing else moves.
#[test]
fn before_the_apply_the_gate_volume_reads_absent() {
    let live = Live {
        gate_volume: None,
        ..w1_as_declared()
    };
    let out = run(&["w-1"], &live_yaml("10.20.0.14", &live), None, None);
    let t = text(&out);
    let absent = lines_with(&out, "ABSENT");
    assert_eq!(absent.len(), 1, "{t}");
    assert!(
        absent
            .iter()
            .any(|l| l.contains("UserVolumeConfig[gate]") && l.contains("500u * GB")),
        "ABSENT prints the declared document:\n{t}"
    );
    assert_eq!(lines_with(&out, "MATCH").len(), 3, "{t}");
    assert_eq!(code(&out), 1, "{t}");
}

/// A /var/mnt/gate kubelet mount found live — the withdrawn entry,
/// applied from the first fragment — is named UNDECLARED, so the hazard
/// review a79746c6 found (the kubelet MkdirAll's it on EPHEMERAL) is
/// visible on every read until it is removed live.
#[test]
fn a_live_gate_mount_the_declaration_withdrew_reads_undeclared() {
    let live = Live {
        gate_mount: true,
        ..w1_as_declared()
    };
    let out = run(&["w-1"], &live_yaml("10.20.0.14", &live), None, None);
    let t = text(&out);
    let undeclared = lines_with(&out, "UNDECLARED");
    assert_eq!(undeclared.len(), 1, "{t}");
    assert!(
        undeclared[0].contains("machine.kubelet.extraMounts[/var/mnt/gate]"),
        "{t}"
    );
}

/// No hostname in the live config means the check cannot tell whose
/// config it read (review a79746c6, finding 7): it still compares, but
/// says UNVERIFIED on its own line and in the summary.
#[test]
fn a_live_config_without_a_hostname_is_named_unverified() {
    let live = Live {
        hostname: None,
        ..w1_as_declared()
    };
    let out = run(&["w-1"], &raw_config(&live), None, None);
    let t = text(&out);
    assert_eq!(lines_with(&out, "UNVERIFIED").len(), 1, "{t}");
    assert!(
        t.contains("node identity UNVERIFIED"),
        "the summary repeats it:\n{t}"
    );
    let named = run(&["w-1"], &raw_config(&w1_as_declared()), None, None);
    assert!(!text(&named).contains("UNVERIFIED"), "{}", text(&named));
}

/// A volume that picked its disk some other way — a serial, the shape
/// the decision refused — is DRIFT, both selectors printed.
#[test]
fn a_gate_volume_chosen_by_another_selector_is_drift_naming_both() {
    let live = Live {
        gate_volume: Some(r#"disk.serial == "S6XNNG0T123456""#),
        ..w1_as_declared()
    };
    let out = run(&["w-1"], &live_yaml("10.20.0.14", &live), None, None);
    let t = text(&out);
    let drift = lines_with(&out, "DRIFT");
    assert_eq!(drift.len(), 1, "{t}");
    assert!(
        drift[0].contains("UserVolumeConfig[gate]")
            && drift[0].contains("500u * GB")
            && drift[0].contains("S6XNNG0T123456"),
        "DRIFT prints both documents:\n{t}"
    );
    assert_eq!(code(&out), 1, "{t}");
}

/// A live user volume no declaration names is the third state:
/// reported, never a failure.
#[test]
fn a_live_user_volume_no_declaration_names_is_undeclared() {
    let dir = scratch("undeclared-volume");
    let patches = dir.join("patches");
    std::fs::create_dir_all(&patches).unwrap();
    std::fs::write(
        patches.join("w-1.yaml"),
        "machine:\n  registries:\n    mirrors:\n      10.20.0.15:3000:\n        endpoints:\n          - http://10.20.0.15:3000\n",
    )
    .unwrap();
    let live = Live {
        hostname: Some("w-1"),
        mirror: true,
        gate_volume: Some(GATE_SELECTOR),
        ..Live::default()
    };
    let out = run(
        &["w-1"],
        &live_yaml("10.20.0.14", &live),
        Some(&patches),
        None,
    );
    let t = text(&out);
    let undeclared = lines_with(&out, "UNDECLARED");
    assert_eq!(undeclared.len(), 1, "{t}");
    assert!(undeclared[0].contains("UserVolumeConfig[gate]"), "{t}");
    assert_eq!(code(&out), 0, "{t}");
}

/// A declaration document that is neither the machine fragment nor a
/// user volume is refused, naming it, rather than silently skipped: a
/// comparator that ignores half its declaration reports a clean node.
#[test]
fn a_declaration_document_the_check_cannot_compare_is_refused() {
    let dir = scratch("unknown-doc");
    let patches = dir.join("patches");
    std::fs::create_dir_all(&patches).unwrap();
    std::fs::write(
        patches.join("w-1.yaml"),
        "machine:\n  registries:\n    mirrors:\n      10.20.0.15:3000:\n        endpoints:\n          - http://10.20.0.15:3000\n---\napiVersion: v1alpha1\nkind: SideroLinkConfig\napiUrl: https://example.test\n",
    )
    .unwrap();
    let live = Live {
        hostname: Some("w-1"),
        mirror: true,
        ..Live::default()
    };
    let out = run(
        &["w-1"],
        &live_yaml("10.20.0.14", &live),
        Some(&patches),
        None,
    );
    let t = text(&out);
    assert_eq!(code(&out), 2, "{t}");
    assert!(t.contains("SideroLinkConfig"), "names the document:\n{t}");
}

#[test]
fn a_file_argument_is_read_instead_of_stdin() {
    let dir = scratch("file-arg");
    let live = w1_as_declared();
    let f = dir.join("w-1.live.yaml");
    std::fs::write(&f, live_yaml("10.20.0.14", &live)).unwrap();
    let out = run(&["w-1", f.to_str().unwrap()], "", None, None);
    assert_eq!(code(&out), 0, "{}", text(&out));
}

#[test]
fn every_control_plane_declaration_matches_its_live_values() {
    // The numbers David read on 2026-09-15: cp-2 40/30, cp-1 and cp-3
    // 50/40, the mirror everywhere. The declarations in the tree must
    // equal them — this is the test that pins the patch files to the
    // measurement they were written from.
    for (node, ip, gc) in [
        ("cp-1", "10.20.0.11", (50, 40)),
        ("cp-2", "10.20.0.12", (40, 30)),
        ("cp-3", "10.20.0.13", (50, 40)),
    ] {
        let live = Live {
            hostname: Some(node),
            image_gc: Some(gc),
            mirror: true,
            ..Live::default()
        };
        let out = run(&[node], &live_yaml(ip, &live), None, None);
        let t = text(&out);
        assert_eq!(code(&out), 0, "{node}: {t}");
        assert_eq!(
            lines_with(&out, "MATCH").len(),
            3,
            "{node}: two GC keys + mirror:\n{t}"
        );
    }
}

#[test]
fn cp2_live_40_30_against_a_declaration_of_50_40_is_drift_naming_both() {
    let dir = scratch("drift");
    let patches = dir.join("patches");
    std::fs::create_dir_all(&patches).unwrap();
    std::fs::write(
        patches.join("cp-2.yaml"),
        "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n      imageGCLowThresholdPercent: 40\n",
    )
    .unwrap();
    let live = Live {
        hostname: Some("cp-2"),
        image_gc: Some((40, 30)),
        ..Live::default()
    };
    let out = run(
        &["cp-2"],
        &live_yaml("10.20.0.12", &live),
        Some(&patches),
        None,
    );
    let t = text(&out);
    let drift = lines_with(&out, "DRIFT");
    assert_eq!(drift.len(), 2, "both GC keys drift:\n{t}");
    assert!(
        drift
            .iter()
            .any(|l| l.contains("imageGCHighThresholdPercent")
                && l.contains("declared \"50\"")
                && l.contains("live \"40\"")),
        "DRIFT prints both values:\n{t}"
    );
    assert!(
        drift
            .iter()
            .any(|l| l.contains("imageGCLowThresholdPercent")
                && l.contains("declared \"40\"")
                && l.contains("live \"30\"")),
        "DRIFT prints both values:\n{t}"
    );
    assert_eq!(code(&out), 1, "{t}");
}

#[test]
fn a_declared_entry_missing_live_is_absent_and_exits_1() {
    let live = Live {
        seed_mount: false,
        ..w1_as_declared()
    };
    let out = run(&["w-1"], &live_yaml("10.20.0.14", &live), None, None);
    let t = text(&out);
    let absent = lines_with(&out, "ABSENT");
    assert_eq!(absent.len(), 1, "{t}");
    assert!(
        absent[0].contains("machine.kubelet.extraMounts[/var/local/gate-seed]"),
        "{t}"
    );
    assert_eq!(code(&out), 1, "{t}");
}

#[test]
fn a_live_entry_no_declaration_names_is_undeclared_and_does_not_fail() {
    // The mirror is live on cp-1 but a declaration that omits it — the
    // third state: reported, never a hard finding (design 16115a17).
    let dir = scratch("undeclared");
    let patches = dir.join("patches");
    std::fs::create_dir_all(&patches).unwrap();
    std::fs::write(
        patches.join("cp-1.yaml"),
        "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n      imageGCLowThresholdPercent: 40\n",
    )
    .unwrap();
    let live = Live {
        hostname: Some("cp-1"),
        image_gc: Some((50, 40)),
        mirror: true,
        other_mount: true,
        ..Live::default()
    };
    let out = run(
        &["cp-1"],
        &live_yaml("10.20.0.11", &live),
        Some(&patches),
        None,
    );
    let t = text(&out);
    let undeclared = lines_with(&out, "UNDECLARED");
    assert_eq!(undeclared.len(), 2, "the mirror and the other mount:\n{t}");
    assert!(
        undeclared.iter().any(
            |l| l.contains("machine.registries.mirrors[10.20.0.15:3000]")
                && l.contains("http://10.20.0.15:3000")
        ),
        "UNDECLARED names the entry and prints its live value:\n{t}"
    );
    assert!(
        undeclared
            .iter()
            .any(|l| l.contains("machine.kubelet.extraMounts[/var/lib/longhorn]")),
        "{t}"
    );
    assert_eq!(code(&out), 0, "UNDECLARED alone is not a failure:\n{t}");
}

#[test]
fn a_live_config_from_another_node_is_refused_not_compared() {
    // A wrong target answers instead of erroring: the live document says
    // which host it is, so the comparator refuses to read cp-1's config
    // as cp-2's.
    let live = Live {
        hostname: Some("cp-1"),
        image_gc: Some((50, 40)),
        mirror: true,
        ..Live::default()
    };
    let out = run(&["cp-2"], &live_yaml("10.20.0.11", &live), None, None);
    let t = text(&out);
    assert_eq!(code(&out), 2, "{t}");
    assert!(
        t.contains("cp-1") && t.contains("cp-2"),
        "names both hosts:\n{t}"
    );
}

#[test]
fn usage_refusals_exit_2() {
    let out = run(&[], "", None, None);
    assert_eq!(code(&out), 2, "no node:\n{}", text(&out));
    assert!(text(&out).contains("usage"), "{}", text(&out));

    let out = run(&["no-such-node"], "machine: {}\n", None, None);
    assert_eq!(
        code(&out),
        2,
        "no declaration for the node:\n{}",
        text(&out)
    );
    assert!(text(&out).contains("no-such-node"), "{}", text(&out));

    let out = run(&["w-1", "/nonexistent/live.yaml"], "", None, None);
    assert_eq!(code(&out), 2, "missing live file:\n{}", text(&out));

    let out = run(&["w-1"], "", None, None);
    assert_eq!(
        code(&out),
        2,
        "empty input carries no machine config:\n{}",
        text(&out)
    );
}

#[test]
fn only_read_classes_judges_the_declaration_alone_and_names_what_it_cannot_read() {
    // node-converge (backlog 9d56c616) applies a declaration and reads
    // its effect back through this check, so it first asks this mode
    // whether every declared entry is one the check can read. The four
    // classes the comparison walks pass; anything else is UNREAD, exit 3.
    let dir = scratch("only-read-classes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("n-1.yaml"),
        "machine:\n  files:\n    - path: /var/local/x\n      content: x\n      op: create\n  kubelet:\n    extraMounts:\n      - destination: /var/local/x\n        source: /var/local/x\n        type: bind\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n  registries:\n    mirrors:\n      10.20.0.15:3000:\n        endpoints:\n          - http://10.20.0.15:3000\n",
    )
    .unwrap();
    let out = run(&["--only-read-classes", "n-1"], "", Some(&dir), None);
    assert_eq!(code(&out), 0, "{}", text(&out));

    std::fs::write(
        dir.join("n-2.yaml"),
        "machine:\n  install:\n    disk: /dev/nvme1n1\n  kubelet:\n    image: x\n    extraConfig:\n      imageGCHighThresholdPercent: 50\ncluster:\n  network:\n    dnsDomain: x\n",
    )
    .unwrap();
    let out = run(&["--only-read-classes", "n-2"], "", Some(&dir), None);
    let t = text(&out);
    assert_eq!(code(&out), 3, "{t}");
    let unread = lines_with(&out, "UNREAD");
    for want in ["machine.install", "machine.kubelet.image", "cluster"] {
        assert!(
            unread.iter().any(|l| l.contains(&format!(" {want} "))),
            "names {want}:\n{t}"
        );
    }
    assert_eq!(unread.len(), 3, "{t}");

    // Under extraConfig, only the kubelet keys the tree declares today
    // (review 9cb9af67, finding 10): an auth setting is UNREAD, exit 3.
    std::fs::write(
        dir.join("n-4.yaml"),
        "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n      authorization:\n        mode: AlwaysAllow\n",
    )
    .unwrap();
    let out = run(&["--only-read-classes", "n-4"], "", Some(&dir), None);
    let t = text(&out);
    assert_eq!(code(&out), 3, "{t}");
    let unread = lines_with(&out, "UNREAD");
    assert_eq!(unread.len(), 1, "{t}");
    assert!(
        unread[0].contains(" machine.kubelet.extraConfig.authorization "),
        "{t}"
    );

    // A UserVolumeConfig the check reads back by name is admitted only
    // with a diskSelector that provably excludes the system disk: a
    // conjunction with `!system_disk` as one of its terms (the
    // coordinator's call on the rebase over disk car 1, 2026-09-30).
    let uvc = |name: &str, provisioning: &str| {
        format!(
            "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n---\napiVersion: v1alpha1\nkind: UserVolumeConfig\nname: {name}\n{provisioning}filesystem:\n  type: xfs\n"
        )
    };
    std::fs::write(
        dir.join("v-ok.yaml"),
        uvc(
            "gate",
            "provisioning:\n  diskSelector:\n    match: '!system_disk && disk.transport == \"nvme\"'\n  minSize: 500GB\n",
        ),
    )
    .unwrap();
    let out = run(&["--only-read-classes", "v-ok"], "", Some(&dir), None);
    assert_eq!(code(&out), 0, "{}", text(&out));
    // The selector is judged by an ALLOWLIST grammar (review be5ba8f7
    // finding 1, the coordinator's call): `term (&& term)*`, each term
    // exactly `!system_disk` or `disk.<ident> <op> <literal>`, exactly
    // one `!system_disk`, and no `(`, `)`, `[`, `]`, `//`, `/*` or
    // newline anywhere. Accepted shapes first, w-1's own among them.
    let sel = |m: &str| {
        format!(
            "provisioning:\n  diskSelector:\n    match: '{}'\n",
            m.replace('\'', "''")
        )
    };
    for (case, m) in [
        ("a-plain", "!system_disk"),
        ("a-string", "!system_disk && disk.transport == \"nvme\""),
        (
            "a-w1",
            "!system_disk && disk.transport == \"nvme\" && disk.size > 500u * GB",
        ),
        ("a-order", "disk.size >= 1u * TB && !system_disk"),
    ] {
        std::fs::write(dir.join(format!("{case}.yaml")), uvc("gate", &sel(m))).unwrap();
        let out = run(&["--only-read-classes", case], "", Some(&dir), None);
        assert_eq!(code(&out), 0, "{case} {m:?}: {}", text(&out));
    }
    let refused_cases: Vec<(&str, String)> =
        vec![
        ("v-any", sel("disk.transport == \"nvme\"")),
        // `||` and `?:`, each alone: a term that is not in the grammar.
        ("v-or", sel("!system_disk && disk.size > 1u || true")),
        ("v-ternary", sel("!system_disk && disk.size > 1u ? true : false")),
        ("v-negated", sel("!(!system_disk && disk.size > 1u)")),
        ("v-twice", sel("!system_disk && !system_disk")),
        // The five bypasses review be5ba8f7 ran through the old split.
        ("v-neg-group", sel("!(disk.size > 0u && !system_disk && true)")),
        ("v-cmp-group", sel("(true && !system_disk && true) != true")),
        ("v-comment", sel("true // && !system_disk")),
        ("v-in-string", sel("disk.model != \" && !system_disk && \"")),
        (
            "v-in-list",
            sel("[true && !system_disk && true, true].exists(x, x)"),
        ),
        // The comment bypass as a block scalar and as a double-quoted
        // scalar carrying a newline.
        (
            "v-comment-block",
            "provisioning:\n  diskSelector:\n    match: |\n      true\n      // && !system_disk\n"
                .to_string(),
        ),
        ("v-block-comment", sel("!system_disk && disk.size > 1u /* x */")),
        ("v-none", "provisioning:\n  minSize: 1GB\n".to_string()),
    ];
    for (case, provisioning) in &refused_cases {
        let case = *case;
        std::fs::write(dir.join(format!("{case}.yaml")), uvc("gate", provisioning)).unwrap();
        let out = run(&["--only-read-classes", case], "", Some(&dir), None);
        let t = text(&out);
        assert_eq!(code(&out), 3, "{case}: {t}");
        let refused = lines_with(&out, "REFUSED");
        assert_eq!(refused.len(), 1, "{case}: {t}");
        assert!(
            refused[0].contains(" UserVolumeConfig[gate] "),
            "{case}: {t}"
        );
        assert!(refused[0].contains("system disk"), "{case}: {t}");
    }
    // The comment bypass as a double-quoted scalar carrying "\n": since
    // review a7fa61ec a double-quoted string is not canonical (its form is
    // a `|-` block), so it is refused one step earlier, exit 2.
    std::fs::write(
        dir.join("v-comment-dq.yaml"),
        uvc(
            "gate",
            "provisioning:\n  diskSelector:\n    match: \"true\\n// && !system_disk\"\n",
        ),
    )
    .unwrap();
    let out = run(
        &["--only-read-classes", "v-comment-dq"],
        "",
        Some(&dir),
        None,
    );
    assert_eq!(code(&out), 2, "v-comment-dq: {}", text(&out));
    assert!(text(&out).contains("canonical"), "{}", text(&out));

    // A second document of any kind but UserVolumeConfig is refused as
    // usage, exit 2 (disk car 1 reads named user volumes and nothing else).
    std::fs::write(
        dir.join("n-3.yaml"),
        "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n---\napiVersion: v1alpha1\nkind: SideroLinkConfig\napiUrl: https://example.invalid\n",
    )
    .unwrap();
    let out = run(&["--only-read-classes", "n-3"], "", Some(&dir), None);
    assert_eq!(code(&out), 2, "{}", text(&out));
    let out = run(&["--only-read-classes"], "", Some(&dir), None);
    assert_eq!(code(&out), 2, "{}", text(&out));
}

/// Review 5ed37182, finding 1: Talos's yaml.v3 counts NEL (U+0085), LS
/// (U+2028) and PS (U+2029) as line breaks and this reader splits on
/// "\n" alone, so a comment ended by one hid a whole document, or a
/// `cluster` key, from --only-read-classes (exit 0). Each separator, in
/// each shape, is refused before any parse — exit 2 naming the line and
/// the codepoint.
fn hidden_by(sep: char) -> [(&'static str, String); 2] {
    [
        (
            "doc",
            format!(
                "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50 # note{sep}---{sep}apiVersion: v1alpha1{sep}kind: UserVolumeConfig{sep}name: evil{sep}provisioning:{sep}  diskSelector:{sep}    match: \"true\"\n"
            ),
        ),
        (
            "key",
            format!(
                "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50 # note{sep}cluster:{sep}  apiServer:{sep}    extraArgs:{sep}      anonymous-auth: \"true\"\n"
            ),
        ),
    ]
}

#[test]
fn a_line_break_this_reader_does_not_split_on_is_refused_before_any_parse() {
    let dir = scratch("unicode-breaks");
    std::fs::create_dir_all(&dir).unwrap();
    for sep in ['\u{0085}', '\u{2028}', '\u{2029}'] {
        let cp = format!("U+{:04X}", sep as u32);
        for (shape, body) in hidden_by(sep) {
            let node = format!("s-{:04x}-{shape}", sep as u32);
            std::fs::write(dir.join(format!("{node}.yaml")), &body).unwrap();
            let out = run(&["--only-read-classes", &node], "", Some(&dir), None);
            let t = text(&out);
            assert_eq!(code(&out), 2, "{cp} hiding a {shape}: {t}");
            assert!(
                t.contains(&cp) && t.contains("line 4"),
                "names the line and codepoint: {t}"
            );
        }
        // The same separator in the LIVE input is refused too.
        let live = format!("machine:\n  network:\n    hostname: w-1{sep}  x: 1\n");
        let out = run(&["w-1"], &live, None, None);
        assert_eq!(code(&out), 2, "{cp} live: {}", text(&out));
        assert!(text(&out).contains(&cp), "{}", text(&out));
    }
    // CR, TAB, a byte-order mark past byte 0, and a C1 control.
    for (name, bad) in [
        ("cr", "\r"),
        ("tab", "\t"),
        ("bom", "\u{feff}"),
        ("c1", "\u{0090}"),
        ("del", "\u{007f}"),
    ] {
        let node = format!("c-{name}");
        std::fs::write(
            dir.join(format!("{node}.yaml")),
            format!("machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50{bad}\n"),
        )
        .unwrap();
        let out = run(&["--only-read-classes", &node], "", Some(&dir), None);
        assert_eq!(code(&out), 2, "{name}: {}", text(&out));
    }
    // A byte-order mark AT byte 0 is the one allowed, and an em dash in a
    // comment is ordinary text: the shipped declarations carry both kinds
    // of non-ASCII the refusal must not touch.
    std::fs::write(
        dir.join("c-lead.yaml"),
        "\u{feff}# a comment — with an em dash\nmachine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n",
    )
    .unwrap();
    let out = run(&["--only-read-classes", "c-lead"], "", Some(&dir), None);
    assert_eq!(code(&out), 0, "{}", text(&out));
    for node in ["cp-1", "cp-2", "cp-3", "w-1"] {
        let out = run(&["--only-read-classes", node], "", None, None);
        assert_eq!(code(&out), 0, "the shipped {node}.yaml: {}", text(&out));
    }
}

/// Review a7fa61ec: the third time this hand reader and Talos's yaml.v3
/// read different STRUCTURE from one text (a `--- {…}` document line was
/// dropped whole). The class is closed, not the instance: a declaration
/// must be in CANONICAL FORM — the input, less comment-only lines,
/// trailing comments and blank lines, byte-identical to the reader's own
/// re-emission of what it parsed — so anything the reader might misread
/// either appears in the one form it provably parsed, or is refused.
const CANONICAL_REFUSED: &[(&str, &str)] = &[
    (
        "flowdoc",
        "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n--- {apiVersion: v1alpha1, kind: UserVolumeConfig, name: evil, provisioning: {diskSelector: {match: \"true\"}, minSize: 1GB}}\n",
    ),
    (
        "flowcluster",
        "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n--- {cluster: {apiServer: {extraArgs: {anonymous-auth: \"true\"}}}}\n",
    ),
    (
        "flowfirst",
        "--- {machine: {install: {wipe: true}}}\n---\nmachine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n",
    ),
    (
        "doc-start-plain-map",
        "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n--- cluster: x\n",
    ),
    (
        "flow-map-value",
        "machine:\n  kubelet:\n    extraConfig: {imageGCHighThresholdPercent: 50}\n",
    ),
    (
        "flow-list-extra",
        "machine:\n  kubelet:\n    extraConfig: [authorization: x]\n",
    ),
    (
        "flow-list",
        "machine:\n  kubelet:\n    extraMounts:\n      - destination: /var/local/x\n        options: [bind, rw]\n",
    ),
    (
        "tag",
        "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: !!int 50\n",
    ),
    (
        "anchor",
        "machine:\n  kubelet:\n    extraConfig: &a\n      imageGCHighThresholdPercent: 50\n",
    ),
    (
        "complex-key",
        "machine:\n  kubelet:\n    extraConfig:\n      ? imageGCHighThresholdPercent\n      : 50\n",
    ),
    (
        "directive",
        "%YAML 1.2\n---\nmachine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n",
    ),
    (
        "doc-end",
        "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n...\ncluster: x\n",
    ),
    (
        "root-at-col-2",
        "  machine:\n    kubelet:\n      extraConfig:\n        imageGCHighThresholdPercent: 50\ncluster: x\n",
    ),
    (
        "seq-after-root",
        "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n- cluster\n",
    ),
    (
        "four-space-indent",
        "machine:\n    kubelet:\n        extraConfig:\n            imageGCHighThresholdPercent: 50\n",
    ),
    (
        "quoted-number",
        "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: \"50\"\n",
    ),
    (
        "leading-doc-marker",
        "---\nmachine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n",
    ),
];

#[test]
fn a_declaration_must_be_in_canonical_form() {
    let dir = scratch("canonical");
    std::fs::create_dir_all(&dir).unwrap();
    for (case, body) in CANONICAL_REFUSED {
        let node = format!("k-{case}");
        std::fs::write(dir.join(format!("{node}.yaml")), body).unwrap();
        for args in [
            vec!["--only-read-classes", node.as_str()],
            vec![node.as_str()],
        ] {
            let live = "machine:\n  network:\n    hostname: x\n";
            let out = run(&args, live, Some(&dir), None);
            assert_eq!(code(&out), 2, "{case} {args:?}: {}", text(&out));
        }
    }
    // Comments, trailing comments and blank lines are not structure: a
    // canonical body carrying all three passes.
    std::fs::write(
        dir.join("k-ok.yaml"),
        "# a comment — kept\nmachine:\n\n  kubelet:  # trailing\n    extraConfig:\n      imageGCHighThresholdPercent: 50 # note\n",
    )
    .unwrap();
    let out = run(&["--only-read-classes", "k-ok"], "", Some(&dir), None);
    assert_eq!(code(&out), 0, "{}", text(&out));
    // The four shipped declarations are canonical.
    for node in ["cp-1", "cp-2", "cp-3", "w-1"] {
        let out = run(&["--only-read-classes", node], "", None, None);
        assert_eq!(code(&out), 0, "the shipped {node}.yaml: {}", text(&out));
    }
}

#[test]
fn a_live_config_with_structure_the_reader_would_drop_is_refused() {
    // Review a7fa61ec: a live `--- {…}` document was never reported. The
    // live config is Talos's own output and is not held to canonical
    // form, but content on a document-marker line, a non-empty flow
    // collection, and lines left after a document's root are refused.
    let base = "machine:\n  network:\n    hostname: w-1\n";
    for (case, live) in [
        (
            "flow-doc",
            format!("{base}--- {{apiVersion: v1alpha1, kind: UserVolumeConfig, name: evil}}\n"),
        ),
        ("seq-after-root", format!("{base}- evil\n")),
        (
            "flow-list",
            "machine:\n  network:\n    hostname: w-1\n  x: [a, b]\n".to_string(),
        ),
    ] {
        let out = run(&["w-1"], &live, None, None);
        assert_eq!(code(&out), 2, "{case}: {}", text(&out));
    }
}

#[test]
fn a_non_mapping_extra_config_is_unread() {
    let dir = scratch("extra-config-list");
    std::fs::create_dir_all(&dir).unwrap();
    // Canonical, and a sequence where the kubelet keys belong.
    std::fs::write(
        dir.join("e-1.yaml"),
        "machine:\n  kubelet:\n    extraConfig:\n      - authorization\n",
    )
    .unwrap();
    let out = run(&["--only-read-classes", "e-1"], "", Some(&dir), None);
    let t = text(&out);
    assert_eq!(code(&out), 3, "{t}");
    assert!(
        lines_with(&out, "UNREAD")
            .iter()
            .any(|l| l.contains("machine.kubelet.extraConfig")),
        "{t}"
    );
}

#[test]
fn a_live_value_cannot_forge_a_verdict_line() {
    // Review 5ed37182 (INFO): show() printed a scalar raw, so a live
    // multi-line value under an imageGC* key could print its own
    // `check-declared: ` summary. Values print through json.dumps now.
    let dir = scratch("forged-line");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("f-1.yaml"),
        "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n",
    )
    .unwrap();
    let live = "machine:\n  network:\n    hostname: f-1\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: \"40\\ncheck-declared: f-1: 9 match, 0 drift, 0 absent, 0 doubled, 0 undeclared — every declared entry matches\"\n";
    let out = run(&["f-1"], live, Some(&dir), None);
    let t = text(&out);
    assert_eq!(code(&out), 1, "the value drifts: {t}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let summaries: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("check-declared: f-1:"))
        .collect();
    assert_eq!(summaries.len(), 1, "only the check's own summary: {t}");
    assert!(summaries[0].contains("1 drift"), "{t}");
}

#[test]
fn a_live_config_it_cannot_decode_is_unreadable_exit_2_never_a_finding() {
    // Review be5ba8f7 finding 2: a YAML-only escape (`\x41`) in a
    // double-quoted scalar crashed the reader with a Python traceback and
    // exit 1 — which every caller reads as "findings". ANY uncaught error
    // is exit 2 with an UNREADABLE line, and no summary line is printed.
    let live = "machine:\n  network:\n    hostname: w-1\n  note: \"\\x41\"\n";
    let out = run(&["w-1"], live, None, None);
    let t = text(&out);
    assert_eq!(code(&out), 2, "{t}");
    assert!(t.contains("UNREADABLE"), "{t}");
    assert!(!t.contains("Traceback"), "no raw traceback: {t}");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("check-declared: w-1:"),
        "no summary line for a config it could not read: {t}"
    );
}

#[test]
fn without_python3_it_refuses_with_78_not_a_verdict() {
    let dir = scratch("no-python");
    let empty = dir.join("bin");
    std::fs::create_dir_all(&empty).unwrap();
    let out = run(
        &["w-1"],
        &live_yaml("10.20.0.14", &w1_as_found()),
        None,
        Some(&empty),
    );
    let t = text(&out);
    assert_eq!(code(&out), 78, "{t}");
    assert!(t.contains("python3"), "{t}");
}

#[test]
fn the_tree_declares_every_node_the_registry_names_as_a_talos_machine() {
    // The patches directory is the declaration; every Talos node in the
    // estate registry's seed (infra/postgres/schema) has a file there.
    for node in ["cp-1", "cp-2", "cp-3", "w-1"] {
        let f = patches_dir().join(format!("{node}.yaml"));
        assert!(f.is_file(), "no declaration at {}", f.display());
        let body = std::fs::read_to_string(&f).unwrap();
        assert!(
            body.starts_with("machine:") || body.contains("\nmachine:\n"),
            "{node}: a patch is a machine-config fragment"
        );
        assert!(
            body.contains("10.20.0.15:3000"),
            "{node}: every node pulls through the forge registry mirror"
        );
        assert!(
            !body.contains("token:") && !body.contains("crt:") && !body.contains("key:"),
            "{node}: no secret lives in a patch"
        );
    }
    let w1 = std::fs::read_to_string(patches_dir().join("w-1.yaml")).unwrap();
    assert_eq!(
        w1.matches("/var/local/gate-seed/.declared-by-talos")
            .count(),
        1,
        "the seed file is declared ONCE — the live double is the drift, not the intent"
    );
    // The manifest points at the declaration instead of restating it
    // (CLAUDE.md §9a: a fact that lives twice).
    let manifest =
        std::fs::read_to_string(repo_root().join("infra/cluster/manifests/gate-seed-local.yaml"))
            .unwrap();
    assert!(
        manifest.contains("infra/cluster/talos/patches/w-1.yaml"),
        "the manifest names the declaration"
    );
    assert!(
        !manifest.contains("extraMounts:"),
        "the manifest no longer restates the fragment"
    );
}

/// THE GATE DISK IS DECLARED BY WHAT IT IS (backlog 52ea56ac, design
/// 8457c07b). One xfs user volume named `gate` — Talos mounts it at
/// /var/mnt/gate — picked by the decided selector, never by a serial
/// or a kernel device name (the forge's devices renumbered when its
/// second NVMe went in, 2026-09-21), and WITHOUT a kubelet extraMount
/// for its path: review a79746c6 found that one makes a missing volume
/// a silent write to EPHEMERAL, and Talos already binds /var/mnt into
/// the kubelet (the proof pod decides whether that suffices). The local
/// PV the gate mounts names the same path, pre-bound to its claim.
#[test]
fn w1_declares_the_gate_volume_by_what_the_disk_is() {
    let w1 = std::fs::read_to_string(patches_dir().join("w-1.yaml")).unwrap();
    let docs: Vec<&str> = w1.split("\n---\n").collect();
    assert_eq!(docs.len(), 2, "the machine fragment and one user volume");
    let uvc = docs[1];
    assert!(uvc.contains("\nkind: UserVolumeConfig\n"), "{uvc}");
    assert!(uvc.contains("\nname: gate\n"), "mounted at /var/mnt/gate");
    assert!(
        uvc.contains(&format!("match: '{GATE_SELECTOR}'")),
        "the decided selector, verbatim:\n{uvc}"
    );
    assert!(uvc.contains("  type: xfs"), "reflink needs xfs");
    let fields: String = uvc
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for named in ["/dev/", "serial", "nvme0", "nvme1", "wwid"] {
        assert!(
            !fields.contains(named),
            "the gate disk is never named by `{named}`:\n{fields}"
        );
    }
    // NO kubelet extraMount for the gate volume (review a79746c6): the
    // kubelet MkdirAll's every extraMounts source at start, so one here
    // puts /var/mnt/gate on EPHEMERAL whenever the volume is absent and
    // the gate writes the install disk silently.
    let machine: String = docs[0]
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !machine.contains("/var/mnt/gate"),
        "w-1 must not declare a /var/mnt/gate kubelet mount:\n{machine}"
    );
    let local =
        std::fs::read_to_string(repo_root().join("infra/cluster/manifests/gate-seed-local.yaml"))
            .unwrap();
    assert!(
        local.contains("path: /var/mnt/gate\n"),
        "the gate PV sits on the declared volume"
    );
    assert!(
        local.contains("claimRef: {namespace: boss-dev, name: gate}"),
        "the gate PV is pre-bound to its claim both ways"
    );
}
