//! The report flip: every service's machine gate is set to `report`, the
//! estate machine token is mounted where its callers read it, and nothing
//! is refused (design 6805c764, car 4; backlog 2710c8fc).
//!
//! WHAT THE FLIP IS. Car 1 mounted the gate on every service binary in
//! mode `off`; cars 2 and 3 taught every caller to stamp the token and
//! the broker to mint it; none of it did anything, because no manifest
//! mounted the mode file or the token. This car mounts both, and the
//! mode it mounts is `report`: the gate admits every request and tallies
//! each one that presents no accepted token (caller, route), which is how
//! enforcement is earned — a clean window, read, never a belief
//! (operator decision (4) on 2710c8fc).
//!
//! THE MACHINE GATE REFUSES NOTHING, and this file is what holds that
//! line:
//!
//!   * its mode is `report` and no manifest says `enforce` for it
//!     anywhere. Enforcement is row C of decision b08725c2 — its own car,
//!     after a 72-hour clean window, on a Pacific-hours train;
//!   * both mounts are `optional: true`. A Secret the broker has not
//!     filled, or a ConfigMap an apply has not reached, mounts EMPTY:
//!     no token is a gate with no slots and a caller that sends none, no
//!     mode file is `off`. The system of record boots either way (DR rule
//!     62dac114), and the converge's instance-secret gate does not skip
//!     an instance for an optional volume (cluster-deploy-lib.sh
//!     manifest_secrets);
//!   * the whole Secret is one directory, never `items:` or `subPath`,
//!     which kubelet never refreshes: a rotation must reach every mount
//!     without a pod roll (design choice 2).
//!
//! THE SECOND KEY (backlog b8e75382, enforce checklist F4 of review
//! 1c2860f4): the policy check's own mode word, `policy-check`, rides the
//! same ConfigMap and the same mount. ROW D's car (design b08725c2)
//! moved it to `enforce`, and this file holds that flip to its one
//! place: the plain `policy-check: enforce` line of that ConfigMap, and
//! nowhere else in any manifest, in any shape. Its rollback road, the
//! kubectl patch over the LAN kube road, is written beside the key and
//! pinned here. THE MACHINE GATE'S HALF IS NOT LOOSENED BY IT: `mode`
//! is still `report`, and `enforce` for it is still refused everywhere,
//! the ConfigMap included — so until row C lands, the policy check's
//! service arm is bounded by an ASSERTED `automation:` id, not a proven
//! one (boss-policy-client `is_sim_identity`: "HOW THAT IS PROVEN TODAY:
//! it is not").
//!
//! WHO HOLDS THE TOKEN is a roster, pinned here, with the reason each
//! workload is on it or deliberately off it. The token lets its holder
//! assert any `x-boss-user` (operator decision (3)), so a workload that
//! runs code from a branch or a package registry does not get it UNLESS
//! it already holds a credential at least as strong, and the roster
//! says so. The playground crawl (`bun install` from npm, then the
//! tree's live suite) and the recovery sheet (a clone of main rendered
//! in Chromium) hold nothing else and would hand the token to that
//! code; they go on sending none, and the report window names them — a
//! miss read on a packet is the design's way to find a caller, and
//! enforcement cannot converge until each is decided.
//!
//! TWO HOLDERS DO RUN SUCH CODE, and are on the roster anyway (review
//! ef2da426 F2): the conductor runs the assembled train tree's gate.sh
//! and every lint as uid 1500, which reads its mount; the dev pod's
//! session runs as uid 0, with `bun install` from npm and crates.io
//! `build.rs` scripts. Both are bounded for THIS phase, and only for it:
//! each already holds the forge WRITE token in the same container
//! (/etc/boss-train), so the class of exposure is not new; `report`
//! grants a token-holder nothing it did not have; and the broker's
//! rotation before enforcement retires any value read in the meantime.
//! Before the enforce car, each needs a decision on the record (the
//! consist lints out of the token-holding container, or the risk
//! accepted), recorded as `before_enforce` on backlog 2710c8fc.
//!
//! THE DEV POD'S COPY (backlog 1876bbdb, INFO-5) is mounted at the one
//! directory `infra/dev/machine-token-dir` names — never boss-core's
//! default, because builders run handler tests on that pod and every
//! stamping client a test builds reads the default — and the pod names
//! it to nobody: only the doors (`infra/dev/boss`, `infra/dev/boss-api`)
//! read that file, and the CLI points the name at an EMPTY directory for
//! every child that runs tree or car code (boss-cli src/door_env.rs) —
//! not merely removed, which falls back to the default and so kept
//! nothing out of reach on a holder that mounts the token there
//! (backlog 844b936e). That keeps
//! ACCIDENTS off the token; hostile code on the pod is the paragraph
//! above.
//!
//! tree-wide pin — it reads every manifest under infra/cluster/manifests
//! and the doors under infra/dev, which no changed-file map attributes to
//! this crate, so every scoped gate runs it (`tree_wide_pins` in
//! infra/gate.sh).

use boss_core::machine_gate::DEFAULT_MODE_FILE;
use boss_core::machine_token::DEFAULT_TOKEN_DIR;
use boss_testing::repo_root;
use std::collections::BTreeMap;

const MANIFESTS: &str = "infra/cluster/manifests";
const DOORS_DIR_FILE: &str = "infra/dev/machine-token-dir";
/// The Secret the broker fills (broker-rotates-the-machine-token.toml).
const SECRET: &str = "boss-machine-token";
/// The ConfigMap holding the mode.
const CONFIG_MAP: &str = "boss-machine-gate";
/// The volume names every manifest gives the two, so a mount line can be
/// matched to its volume by text.
const TOKEN_VOLUME: &str = "machine-token";
const MODE_VOLUME: &str = "machine-gate";

/// Every manifest that mounts the token, and why it holds it.
const HOLDERS: &[(&str, &str)] = &[
    (
        "boss.yaml",
        "every service's gate reads the slots, and every service, the gateway and the dispatcher's broker stamp `current`",
    ),
    (
        "boss-audit-integrity.yaml",
        "a chore: its wrap and step (infra/boss-maintenance-wrap.sh, boss-step.sh) open and close its packet",
    ),
    ("boss-conservation-invariants.yaml", "a chore, as above"),
    ("boss-files-gc.yaml", "a chore, as above"),
    ("boss-ledger-recognize.yaml", "a chore, as above"),
    ("boss-ledger-replay-check.yaml", "a chore, as above"),
    ("boss-messages-events-purge.yaml", "a chore, as above"),
    ("boss-search-reindex.yaml", "a chore, as above"),
    ("boss-views-catchup.yaml", "a chore, as above"),
    (
        "boss-backup.yaml",
        "the nightly backup's open and close legs are the chore pair",
    ),
    (
        "boss-break-glass-deposit.yaml",
        "a chore through boss-chore.sh",
    ),
    (
        "boss-conductor.yaml",
        "the train conductor writes the record all day through the boss CLI — and runs the \
         assembled train tree's gate.sh and lints as uid 1500, which reads the mount; bounded \
         because it already holds the forge write token, report grants nothing, and a rotation \
         before enforce retires what was read (decision owed: before_enforce on 2710c8fc)",
    ),
    (
        "boss-dev.yaml",
        "the dev pod's doors, boss-api and the boss shim — at the doors' own directory; the \
         session runs as uid 0 with bun install and build.rs, which can read it; bounded \
         because it already holds the forge write token, report grants nothing, and a rotation \
         before enforce retires what was read (decision owed: before_enforce on 2710c8fc)",
    ),
];

/// Manifests that must NOT mount it, and why.
const NEVER: &[(&str, &str)] = &[
    (
        "boss-playground-crawl.yaml",
        "runs `bun install` from npm and the tree's live suite in the chore's own container",
    ),
    (
        "boss-recovery-sheet.yaml",
        "renders a fresh clone of main in Chromium in the chore's own container",
    ),
    (
        "gate-seed-local.yaml",
        "the gate's seed: gates run every car's code before review",
    ),
];

fn manifests() -> BTreeMap<String, String> {
    let dir = repo_root().join(MANIFESTS);
    std::fs::read_dir(&dir)
        .expect("the manifests")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&p).expect("a manifest"),
            )
        })
        .collect()
}

fn live_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter(|l| !l.trim_start().starts_with('#'))
}

/// `key: value` inside a flow mapping line, e.g. `mountPath` out of
/// `- {name: machine-token, mountPath: /etc/boss/machine-token}`.
fn flow_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let at = line.find(&format!("{key}: "))? + key.len() + 2;
    let rest = &line[at..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim().trim_matches('"'))
}

/// The mount lines naming `volume`, as `(mountPath, whole line)`.
fn mounts<'a>(text: &'a str, volume: &str) -> Vec<(&'a str, &'a str)> {
    live_lines(text)
        .filter(|l| flow_value(l, "name") == Some(volume) && l.contains("mountPath:"))
        .map(|l| (flow_value(l, "mountPath").unwrap_or(""), l))
        .collect()
}

fn doors_dir() -> String {
    std::fs::read_to_string(repo_root().join(DOORS_DIR_FILE))
        .expect("infra/dev/machine-token-dir")
        .trim()
        .to_string()
}

/// The mode keys the ConfigMap carries, each the file name its reader
/// opens inside the one mounted directory: the machine gate's own, and
/// the policy check's (F7 of backlog b8e75382, row D of design b08725c2,
/// "beside the machine-gate mode and read the same way"). Read off the
/// readers' constants, never spelled here.
fn mode_keys() -> [&'static str; 2] {
    let key = |file: &'static str| file.rsplit('/').next().unwrap();
    [
        key(DEFAULT_MODE_FILE),
        key(boss_policy::check_mode::DEFAULT_MODE_FILE),
    ]
}

fn mode_config_map(boss: &str) -> &str {
    boss.split("\n---")
        .find(|d| d.contains("kind: ConfigMap") && d.contains(&format!("name: {CONFIG_MAP}")))
        .unwrap_or_else(|| panic!("boss.yaml declares ConfigMap {CONFIG_MAP}"))
}

#[test]
fn the_machine_gate_reports_and_only_the_policy_check_enforces() {
    let all = manifests();
    let doc = mode_config_map(&all["boss.yaml"]);
    let [gate, check] = mode_keys();
    for (key, want, why) in [
        (
            gate,
            "report",
            "what the machine gate would refuse is admitted and tallied (design 6805c764 car 4); \
             its `enforce` is row C of decision b08725c2, its own car, after a 72-hour clean \
             window, on a Pacific-hours train — NOT the policy check's car",
        ),
        (
            check,
            "enforce",
            "row D of decision b08725c2 (F7 of backlog b8e75382): an unsigned check and a \
             service whose Read on policy-rule has lapsed are refused. The way back is the \
             word `report` on this line, by the kubectl road written above it",
        ),
    ] {
        let modes: Vec<&str> = live_lines(doc)
            .filter_map(|l| l.trim().strip_prefix(&format!("{key}:")))
            .map(|v| v.trim().trim_matches('"'))
            .collect();
        assert_eq!(modes, [want], "{CONFIG_MAP}'s `{key}` is `{want}`: {why}");
    }
    for (name, text) in &all {
        let lines: Vec<&str> = live_lines(text).collect();
        if let Some(line) = sets_enforce(&lines, &[gate]) {
            panic!(
                "{name}: `{line}` — no manifest sets the MACHINE gate to enforce: that is row \
                 C's car, and row D's flip of `{check}` does not carry it"
            );
        }
        // The policy check's `enforce` lives on ONE line, counted above:
        // the plain key of the ConfigMap. Any other spelling, in this
        // file or another, is a second fact the rollback would miss.
        let outside = text.replace(doc, "");
        let lines: Vec<&str> = live_lines(&outside).collect();
        if let Some(line) = sets_enforce(&lines, &[check]) {
            panic!(
                "{name}: `{line}` — `{check}: enforce` is spelled once, in ConfigMap \
                 {CONFIG_MAP}, where its rollback road names it"
            );
        }
        for env in [
            "BOSS_MACHINE_GATE_MODE_FILE",
            boss_policy::check_mode::MODE_FILE_ENV,
        ] {
            assert!(
                !text.contains(env),
                "{name}: {env} — each mode file is read at its reader's default, in the one \
                 mounted directory; a second path would be a second fact"
            );
        }
    }
}

/// Checklist F4 of backlog b8e75382: the rollback of the policy check's
/// enforce flip needs a road that passes neither the jobs API nor the
/// forge, because its misfire refuses every signed-in write. The road is
/// written beside the ConfigMap, and names that ConfigMap and that key,
/// so it cannot drift from what it rolls back.
#[test]
fn the_policy_checks_way_back_is_written_beside_its_key() {
    let all = manifests();
    let doc = mode_config_map(&all["boss.yaml"]);
    let key = mode_keys()[1];
    let road = doc
        .lines()
        .find(|l| l.trim_start().starts_with("#") && l.contains("kubectl"))
        .unwrap_or_else(|| panic!("{CONFIG_MAP} carries its rollback road as a kubectl line"));
    for part in [
        "-n boss".to_string(),
        format!("patch configmap {CONFIG_MAP}"),
        format!("\"{key}\":\"report\""),
    ] {
        assert!(road.contains(&part), "the road names {part}: {road}");
    }
}

/// The first line in `lines` (comments already dropped) that gives one
/// of `keys` (from [`mode_keys`]: `mode`, `policy-check`) a value the gate's
/// own parser reads as `enforce` — in any shape a manifest can spell it
/// (review ef2da426 F3): YAML block or flow, JSON, any case, any quotes,
/// or a block scalar (`mode: |`) whose next non-blank line holds the
/// word. The value goes through `Mode::parse`, the one parse every mode
/// word takes, so this can never be narrower than what a mounted file
/// would enforce.
fn sets_enforce(lines: &[&str], keys: &[&str]) -> Option<String> {
    use boss_core::machine_gate::Mode;
    let norm = |l: &str| l.to_ascii_lowercase().replace(['"', '\''], "");
    for (i, raw) in lines.iter().enumerate() {
        let line = norm(raw);
        let found = keys
            .iter()
            .copied()
            .flat_map(|key| line.match_indices(key).map(move |(at, _)| (at, key.len())))
            .collect::<Vec<_>>();
        for (at, len) in found {
            let before = line[..at].chars().last();
            if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                continue; // defaultmode:, some_mode:, gate-mode:
            }
            let Some(rest) = line[at + len..].trim_start().strip_prefix(':') else {
                continue;
            };
            let end = rest.find([',', '}']).unwrap_or(rest.len());
            let mut value = rest[..end].trim().to_string();
            if value.starts_with('|') || value.starts_with('>') {
                value = lines[i + 1..]
                    .iter()
                    .map(|l| norm(l).trim().to_string())
                    .find(|l| !l.is_empty())
                    .unwrap_or_default();
            }
            if Mode::parse(Some(&value)).0 == Mode::Enforce {
                return Some(raw.trim().to_string());
            }
        }
    }
    None
}

#[test]
fn the_enforce_matcher_reads_every_shape_the_parser_would() {
    for shape in [
        vec!["  mode: enforce"],
        vec!["  mode: Enforce"],
        vec!["  mode: \"ENFORCE\""],
        vec!["  mode: ' enforce '"],
        vec!["  {\"mode\": \"enforce\"}"],
        vec!["data: {mode: enforce, other: x}"],
        vec!["  mode: |", "", "    Enforce"],
        vec!["  mode: >-", "    enforce"],
        vec!["  policy-check: enforce"],
        vec!["  policy-check: \"Enforce\""],
        vec!["data: {mode: report, policy-check: enforce}"],
        vec!["  policy-check: |", "    enforce"],
    ] {
        assert!(sets_enforce(&shape, &mode_keys()).is_some(), "{shape:?}");
    }
    for shape in [
        vec!["  mode: report"],
        vec!["  mode: off"],
        vec!["  secret: {secretName: x, optional: true, defaultMode: 0440}"],
        vec!["  gate-mode: enforce"],
        vec!["  mode: enforce-later"],
        vec!["  mode: |", "    report"],
        vec!["  policy-check: report"],
        vec!["  x-policy-check: enforce"],
    ] {
        assert!(sets_enforce(&shape, &mode_keys()).is_none(), "{shape:?}");
    }
    // Each key is judged alone, so the policy check's flip (row D) cannot
    // carry the machine gate's (row C) past the pin, in either direction.
    let [gate, check] = mode_keys();
    let both = ["data: {mode: enforce, policy-check: report}"];
    assert!(sets_enforce(&both, &[gate]).is_some());
    assert!(sets_enforce(&both, &[check]).is_none());
    let flipped = ["  mode: report", "  policy-check: enforce"];
    assert!(sets_enforce(&flipped, &[gate]).is_none());
    assert!(sets_enforce(&flipped, &[check]).is_some());
}

#[test]
fn the_services_read_the_mode_where_the_gate_looks() {
    let boss = &manifests()["boss.yaml"];
    let dir = DEFAULT_MODE_FILE.rsplit_once('/').unwrap().0;
    let vol = live_lines(boss)
        .find(|l| l.contains(&format!("name: {CONFIG_MAP}")) && l.contains("configMap"))
        .unwrap_or_else(|| panic!("boss.yaml has a configMap volume of {CONFIG_MAP}"));
    assert_eq!(
        flow_value(vol, "optional"),
        Some("true"),
        "optional: a missing ConfigMap reads as `off`, never a pod that cannot start: {vol}"
    );
    let m = mounts(boss, MODE_VOLUME);
    assert_eq!(
        m.len(),
        1,
        "one mount of {MODE_VOLUME}, on the boss container: {m:?}"
    );
    assert_eq!(m[0].0, dir, "mounted whole at {dir}: {}", m[0].1);
    assert!(!m[0].1.contains("subPath"), "a subPath is never refreshed");
}

#[test]
fn the_token_is_mounted_whole_optional_and_where_each_holder_reads_it() {
    let all = manifests();
    let doors = doors_dir();
    let mut holders = Vec::new();
    for (name, text) in &all {
        let volumes: Vec<&str> = live_lines(text)
            .filter(|l| l.contains(&format!("secretName: {SECRET}")))
            .collect();
        if volumes.is_empty() {
            assert!(
                mounts(text, TOKEN_VOLUME).is_empty(),
                "{name} mounts {TOKEN_VOLUME} without the Secret volume"
            );
            continue;
        }
        holders.push(name.clone());
        for v in &volumes {
            assert_eq!(
                flow_value(v, "optional"),
                Some("true"),
                "{name}: optional — an unfilled Secret mounts empty, never a pod that cannot \
                 start (DR rule 62dac114): {v}"
            );
            assert!(
                !v.contains("items"),
                "{name}: the whole Secret, never items: {v}"
            );
            let mode = flow_value(v, "defaultMode").unwrap_or("0644");
            let other_reads = mode.ends_with('4') || mode.ends_with('6');
            assert!(
                other_reads || text.contains("fsGroup: 1500"),
                "{name}: defaultMode {mode} is readable by uid 1500 only through the pod's \
                 fsGroup 1500, which this manifest does not set"
            );
        }
        let m = mounts(text, TOKEN_VOLUME);
        assert!(
            !m.is_empty(),
            "{name} declares the Secret volume and mounts it nowhere"
        );
        let want = if name == "boss-dev.yaml" {
            doors.as_str()
        } else {
            DEFAULT_TOKEN_DIR
        };
        for (path, line) in &m {
            assert_eq!(*path, want, "{name}: {line}");
            assert!(
                !line.contains("subPath"),
                "{name}: a subPath is never refreshed"
            );
        }
        if name != "boss-dev.yaml" {
            let hosts = live_lines(text)
                .filter(|l| l.contains("BOSS_MACHINE_TOKEN_HOSTS"))
                .count();
            assert!(
                hosts >= m.len(),
                "{name}: {} mount(s) of the token and {hosts} BOSS_MACHINE_TOKEN_HOSTS — every \
                 container that holds the token names the hosts it may stamp, or it reaches its \
                 Service names unstamped (the car-4 checklist on 2710c8fc)",
                m.len()
            );
        }
    }
    let want: Vec<String> = {
        let mut v: Vec<String> = HOLDERS.iter().map(|(n, _)| n.to_string()).collect();
        v.sort();
        v
    };
    assert_eq!(
        holders, want,
        "the manifests that mount {SECRET} are the roster HOLDERS names, each with its reason"
    );
    for (name, why) in NEVER {
        assert!(
            all.contains_key(*name),
            "{name} is gone — drop it from NEVER"
        );
        assert!(
            !holders.iter().any(|h| h == name),
            "{name} must not hold the token: {why}"
        );
    }
}

/// INFO-5: the dev pod's copy is off boss-core's default, and the pod
/// names it to nothing but the doors.
#[test]
fn the_dev_pods_copy_is_named_only_to_the_doors() {
    let doors = doors_dir();
    assert!(doors.starts_with('/'), "{DOORS_DIR_FILE}: absolute");
    assert_ne!(
        doors, DEFAULT_TOKEN_DIR,
        "{DOORS_DIR_FILE} must not be boss-core's default, which every test process reads"
    );
    assert!(
        !doors.starts_with(&format!("{DEFAULT_TOKEN_DIR}/")),
        "nor inside it"
    );
    let dev = &manifests()["boss-dev.yaml"];
    assert!(
        !live_lines(dev).any(|l| l.contains("BOSS_MACHINE_TOKEN_DIR")),
        "boss-dev.yaml must not name the directory in the pod's environment: every process \
         on the pod would inherit it, builders' tests included"
    );
    for door in ["infra/dev/boss", "infra/dev/boss-api"] {
        let text = std::fs::read_to_string(repo_root().join(door)).expect("a door");
        assert!(
            text.contains("machine-token-dir"),
            "{door} reads {DOORS_DIR_FILE}"
        );
    }
}

/// No manifest reads the old key. The env var it fed is deleted as a
/// source (design choice 2), and the pin every_shell_sender_reads_the_
/// machine_token_from_its_mount.rs holds the scripts to the mount.
#[test]
fn no_manifest_reads_the_machine_token_key_of_boss_secrets() {
    for (name, text) in manifests() {
        for l in live_lines(&text) {
            assert!(
                !(l.contains("key: machine-token") || l.contains("key: \"machine-token\"")),
                "{name}: `{}` — the token is the mounted {SECRET} Secret, never a key of \
                 boss-secrets",
                l.trim()
            );
        }
    }
}
