//! The probe-reader credential's slots are mounted where every service's
//! machine gate reads them, in one pod, and nowhere else (design
//! b35c22b4; backlog d26515c5, the server-mount car).
//!
//! WHAT WAS MEASURED (closure read of d26515c5, origin/main 53c151631,
//! 2026-10-07). The gate has read a second slot set from
//! `/etc/boss/probe-reader` since car 93af0e4c, and the CLI door that
//! presents it landed in #972 — and nothing mounted the directory, so
//! both were inert: 80 `would_refuse` facts from
//! `automation:run-car-probe-reader` presenting `none` in 72 hours, and
//! every recorded probe read refused the day the gate enforces (row C of
//! decision b08725c2).
//!
//! WHY ONE MOUNT IS EVERY PORT. The door vouches PER PORT: it asks each
//! port's `/api/machine-gate/accepts` what it makes of the credential and
//! answers 502 for a port whose gate does not name it a reader slot,
//! which reads as a FAILED probe against correct code (delta review
//! 991bb439, N1). So a gated port that does not read the directory is a
//! broken proof, not a narrower grant. Every service binary runs under
//! the one launcher in the one `boss` container (boss.yaml's header;
//! `launcher_roster_agreement` in boss-ports), and every gate takes its
//! paths from `MountedFiles::from_env`, which always names a reader
//! directory. One mount at the default path, with no env var naming a
//! second, therefore reaches every gate — and this file holds each of
//! those three facts, because they live in three places (CLAUDE.md §9a).
//!
//! THE GATEWAY (4443) is the one row of `infra/forge/sor-ports.env` with
//! no gate (`machine_gate::UNGATED`): it answers 404 on the accepts
//! route — measured again on 2026-10-07 through the pod's door — so the
//! door could never vouch there. No path routes a probe to it
//! (`probe-bin/sor-routes.sh`), and `boss-gateway-read` reads it as a
//! stranger, with no credential, by decision (b35c22b4).
//!
//! WHO HOLDS IT. Only the pod that serves gated ports. A caller never
//! needs this mount: the one client is `boss prove` on a proving host,
//! which reads a deposited file (a later car), and no workload that runs
//! branch or probe code gets the directory. The mount grants the boss
//! container nothing it lacked: the same container already mounts the
//! estate token, which reads AND writes.
//!
//! THE PLAYGROUND renders this same manifest, so its pod mounts the same
//! optional volume — and nothing fills it. The broker's grant below is
//! in namespace `boss` alone, no rule names another namespace, and the
//! credential is prod's forge reading prod's record; the playground's
//! reader set stays empty and matches nothing, exactly as its estate
//! token does.
//!
//! NOTHING IS REFUSED BY THIS MOUNT. The volume is `optional: true`: the
//! Secret does not exist until a broker rule declares it (the converge
//! creates a declared Secret empty, `ensure_declared_secrets`), and an
//! absent or unfilled Secret is an empty directory, an empty slot set,
//! and a gate that answers every caller as it did before.
//!
//! tree-wide pin — it reads the manifests, the launcher and the forge's
//! port table, which no changed-file map attributes to this crate, so
//! every scoped gate runs it (`tree_wide_pins` in infra/gate.sh).

use boss_core::machine_gate::{DEFAULT_READER_DIR, READER_DIR_ENV, is_gated};
use boss_core::machine_token::DEFAULT_TOKEN_DIR;
use boss_testing::repo_root;
use std::collections::BTreeMap;

const MANIFESTS: &str = "infra/cluster/manifests";
const BROKER: &str = "boss-credential-broker.yaml";
const LAUNCHER: &str = "infra/oss-quickstart/services-launcher.sh";
const SOR_PORTS: &str = "infra/forge/sor-ports.env";
const SOR_ROUTES: &str = "infra/forge/probe-bin/sor-routes.sh";
const GATE_SOURCE: &str = "crates/core/boss-core/src/machine_gate.rs";
/// The Secret holding the reader's `current`, `next` and `previous`.
const SECRET: &str = "boss-probe-reader";
/// The volume name the manifest gives it.
const VOLUME: &str = "probe-reader";
/// The estate token's volume, mounted beside it in the same container.
const TOKEN_VOLUME: &str = "machine-token";

/// The one manifest that mounts it, and why.
const HOLDERS: &[(&str, &str)] = &[(
    "boss.yaml",
    "every service's gate reads the reader slots, and every gated port is served from this pod",
)];

fn read(path: &str) -> String {
    std::fs::read_to_string(repo_root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn manifests() -> BTreeMap<String, String> {
    std::fs::read_dir(repo_root().join(MANIFESTS))
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

/// `key: value` inside a flow mapping line.
fn flow_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let at = line.find(&format!("{key}: "))? + key.len() + 2;
    let rest = &line[at..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim().trim_matches('"'))
}

/// The mount lines naming `volume`.
fn mounts<'a>(text: &'a str, volume: &str) -> Vec<&'a str> {
    live_lines(text)
        .filter(|l| flow_value(l, "name") == Some(volume) && l.contains("mountPath:"))
        .collect()
}

#[test]
fn the_boss_pod_mounts_the_reader_slots_whole_optional_and_read_only() {
    let all = manifests();
    let boss = &all["boss.yaml"];
    let deployment = boss
        .split("\n---")
        .find(|d| d.contains("\nkind: Deployment\n") && d.contains("\n  name: boss\n"))
        .expect("boss.yaml declares Deployment boss");

    let volumes: Vec<&str> = live_lines(deployment)
        .filter(|l| l.contains(&format!("secretName: {SECRET}")))
        .collect();
    assert_eq!(
        volumes.len(),
        1,
        "Deployment boss has one volume of Secret {SECRET}: {volumes:?}"
    );
    let v = volumes[0];
    assert_eq!(
        flow_value(v, "optional"),
        Some("true"),
        "optional — the Secret does not exist until a broker rule declares it, and the system \
         of record boots without it (DR rule 62dac114): {v}"
    );
    assert!(
        !v.contains("items"),
        "the whole Secret, never items — a slot must reach the mount without an edit here: {v}"
    );
    assert_eq!(
        flow_value(v, "defaultMode"),
        Some("0440"),
        "0440 + the pod fsGroup, as the estate token beside it: {v}"
    );
    assert!(
        deployment.contains("fsGroup: 1500"),
        "defaultMode 0440 is readable by the boss uid only through the pod's fsGroup"
    );

    let m = mounts(deployment, VOLUME);
    assert_eq!(m.len(), 1, "one mount of {VOLUME}: {m:?}");
    assert_eq!(
        flow_value(m[0], "mountPath"),
        Some(DEFAULT_READER_DIR),
        "at boss-core's default, where every gate looks: {}",
        m[0]
    );
    assert_eq!(flow_value(m[0], "readOnly"), Some("true"), "{}", m[0]);
    assert!(
        !m[0].contains("subPath"),
        "a subPath is never refreshed, and a rotation must reach the gates with no pod roll"
    );

    // The same container as the estate token: the one that runs every
    // service. Two directories, neither inside the other, so a slot file
    // of one credential can never be read as the other's.
    let token = mounts(deployment, TOKEN_VOLUME);
    assert_eq!(token.len(), 1, "{token:?}");
    let block_of = |line: &str| {
        let at = deployment.find(line).expect("the mount line");
        deployment[..at].rfind("volumeMounts:").expect("its block")
    };
    assert_eq!(
        block_of(m[0]),
        block_of(token[0]),
        "the reader slots are mounted in the container that holds the estate token's"
    );
    assert_ne!(DEFAULT_READER_DIR, DEFAULT_TOKEN_DIR);
    for (a, b) in [
        (DEFAULT_READER_DIR, DEFAULT_TOKEN_DIR),
        (DEFAULT_TOKEN_DIR, DEFAULT_READER_DIR),
    ] {
        assert!(!a.starts_with(&format!("{b}/")), "{a} is inside {b}");
    }
}

#[test]
fn only_the_pod_that_serves_gated_ports_mounts_it() {
    let mut holders = Vec::new();
    for (name, text) in manifests() {
        let names_it = live_lines(&text).any(|l| {
            l.contains(&format!("secretName: {SECRET}"))
                || flow_value(l, "mountPath").is_some_and(|p| {
                    p == DEFAULT_READER_DIR || p.starts_with(&format!("{DEFAULT_READER_DIR}/"))
                })
        });
        if names_it {
            holders.push(name);
        }
    }
    let want: Vec<String> = HOLDERS.iter().map(|(n, _)| n.to_string()).collect();
    assert_eq!(
        holders, want,
        "the manifests that mount {SECRET} are the roster HOLDERS names — a gate reads these \
         slots and a caller never does; no chore, conductor, dev pod or gate runner holds them"
    );
}

/// Every gate reads the ONE mounted directory: `mount` takes its paths
/// from `from_env`, `from_env` names the reader directory by this env var
/// or the default, and nothing a pod runs sets the env var.
#[test]
fn every_gate_reads_the_mounted_directory_and_nothing_names_a_second() {
    let gate = read(GATE_SOURCE);
    let body_of = |signature: &str| {
        let at = gate
            .find(signature)
            .unwrap_or_else(|| panic!("{GATE_SOURCE} defines `{signature}`"));
        // To the first closing brace at function or method depth: far
        // enough for the one line each assertion below reads.
        let rest = &gate[at..];
        let end = [rest.find("\n}\n"), rest.find("\n    }\n")]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(rest.len());
        &rest[..end]
    };
    assert!(
        // The free function, at column 0 — `ModeSwitch::mount` is a
        // method of another reader and is indented.
        body_of("\npub fn mount(").contains("MountedFiles::from_env()"),
        "every service mounts its gate through `mount`, which reads the paths from the \
         environment"
    );
    assert!(
        body_of("pub fn from_env() -> Self")
            .contains(".with_reader_dir(var(READER_DIR_ENV, DEFAULT_READER_DIR))"),
        "and that reading always names the reader directory"
    );

    for (name, text) in manifests() {
        assert!(
            !text.contains(READER_DIR_ENV),
            "{name}: {READER_DIR_ENV} — the reader slots are read at boss-core's default, in \
             the one mounted directory; a second path is a gate that reads no slots, and a \
             502 from the probe door on that port"
        );
    }
    let quickstart = repo_root().join("infra/oss-quickstart");
    for entry in std::fs::read_dir(&quickstart).expect("infra/oss-quickstart") {
        let path = entry.expect("an entry").path();
        if !path.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        assert!(
            !text.contains(READER_DIR_ENV),
            "{}: {READER_DIR_ENV} — the launcher starts every service with one environment, \
             and none of it names a second reader directory",
            path.display()
        );
    }
}

/// `name=port` rows of the forge's port table.
fn sor_ports() -> Vec<(String, u16)> {
    read(SOR_PORTS)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (name, port) = l
                .split_once('=')
                .unwrap_or_else(|| panic!("{SOR_PORTS}: {l}"));
            (
                name.to_string(),
                port.parse().unwrap_or_else(|_| panic!("{SOR_PORTS}: {l}")),
            )
        })
        .collect()
}

#[test]
fn every_gated_port_a_probe_reads_is_served_from_that_pod() {
    let launcher = read(LAUNCHER);
    let start = launcher.find("SERVICES=(").expect("the launcher's roster");
    let roster = &launcher[start..start + launcher[start..].find("\n)").expect("its end")];
    let launched: Vec<&str> = roster
        .lines()
        .filter_map(|l| l.trim().strip_prefix('"')?.strip_suffix('"'))
        .collect();
    let routes = read(SOR_ROUTES);

    let ports = sor_ports();
    assert!(
        ports.len() >= 10,
        "read only {} rows of {SOR_PORTS}",
        ports.len()
    );
    let mut ungated = Vec::new();
    for (name, port) in &ports {
        assert_eq!(
            boss_ports::prod(name),
            *port,
            "{SOR_PORTS}: `{name}` is the boss-ports row of that name"
        );
        if !is_gated(name) {
            ungated.push(name.as_str());
            assert!(
                !live_lines(&routes).any(|l| l.contains(&format!("service={name} "))
                    || l.trim_end().ends_with(&format!("service={name}"))),
                "{SOR_ROUTES} routes a probe path to `{name}`, which mounts no gate: its \
                 accepts route answers 404, the probe door cannot vouch there, and the read \
                 is a 502"
            );
            continue;
        }
        assert!(
            boss_ports::launcher_binaries(name)
                .iter()
                .any(|b| launched.contains(&b.as_str())),
            "`{name}` (port {port}) is a gated port a probe may read, and {LAUNCHER} does not \
             start it: it is not served from the pod that mounts {SECRET}, so the probe door \
             is refused there"
        );
    }
    assert_eq!(
        ungated,
        ["gateway"],
        "the gateway is the one probe-readable port with no gate (machine_gate::UNGATED)"
    );
}

/// The broker's grant on the Secret: by name, get and patch, in prod's
/// namespace alone, bound to the boss pod's service account — the shape
/// `a_declared_broker_secret_is_created_empty.rs` demands of every
/// Secret a broker rule declares, held here before that rule exists so
/// the rule's car edits no Role.
#[test]
fn the_broker_may_fill_it_in_prod_and_nowhere_else() {
    let manifest = read(&format!("{MANIFESTS}/{BROKER}"));
    let docs: Vec<&str> = manifest.split("\n---").collect();
    let grants: Vec<&&str> = docs
        .iter()
        .filter(|d| {
            d.contains("\nkind: Role\n")
                && live_lines(d).any(|l| {
                    l.trim()
                        .strip_prefix("resourceNames: [")
                        .and_then(|l| l.strip_suffix(']'))
                        .is_some_and(|l| l.split(',').any(|n| n.trim() == SECRET))
                })
        })
        .collect();
    assert_eq!(
        grants.len(),
        1,
        "one Role names {SECRET}: prod's. No playground and no dev-pod copy — the credential \
         is prod's forge reading prod's record"
    );
    let role = grants[0];
    assert!(role.contains("\n  namespace: boss\n"), "{role}");
    let rules: Vec<&str> = live_lines(role)
        .filter(|l| l.trim_start().starts_with("verbs:"))
        .collect();
    assert_eq!(
        rules,
        ["    verbs: [get, patch]"],
        "one rule, get and patch: the broker reads which slot holds which packet's value and \
         writes a slot; it cannot create, list or delete"
    );
    assert!(
        live_lines(role).any(|l| l.trim() == format!("resourceNames: [{SECRET}]")),
        "the rule names this Secret alone"
    );
    let role_name = role
        .lines()
        .find_map(|l| l.strip_prefix("  name: "))
        .expect("the Role's name");
    let boss = read(&format!("{MANIFESTS}/boss.yaml"));
    let pod_sa = boss
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("serviceAccountName: "))
        .expect("the boss Deployment names its service account");
    assert!(
        docs.iter().any(|d| d.contains("\nkind: RoleBinding\n")
            && d.contains("\n  namespace: boss\n")
            && d.contains(&format!("\n  name: {role_name}\n"))
            && d.contains(&format!("    name: {pod_sa}\n    namespace: boss"))),
        "Role {role_name} is bound to the boss pod's service account `{pod_sa}`, in boss"
    );
}
