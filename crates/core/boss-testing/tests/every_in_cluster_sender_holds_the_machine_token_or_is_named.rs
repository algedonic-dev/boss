//! Every container under `infra/cluster/manifests` that sends to a port
//! the machine gate guards either HOLDS the estate machine token the one
//! way a pod may — the Secret mounted read-only and optional, the reader
//! from the boss image — or is named here with the reason it does not
//! (backlog 37742794, part of urgent 2710c8fc; David's answers to
//! design-doc c8502e17, 2026-10-07).
//!
//! WHY. Every service's machine gate records, and under `enforce` will
//! refuse, a request with no token. Three pins already hold the senders
//! they can see: the shell scripts under infra/, the Rust clients, and a
//! curl typed into a manifest's `args:`. None of them reads a POD: a
//! chore whose script stamps perfectly sends bare from a container with
//! no mount, and that is what the report window showed on 2026-10-07 —
//! the dev pod's reclaim sidecar (66 would-refuse facts in 26 h), the
//! recovery sheet (8) and the playground crawl's wrap pair, each a
//! stamping script in a container that mounted nothing.
//!
//! WHAT A SENDER IS, read off the parsed manifest and never a line scan:
//! a container any of whose string values (an env value, an argv word)
//! names the host of a Service the machine door fronts — the `*-internal`
//! Services of the `boss` namespace, read from their own manifests — or
//! which CHECKOUT_SENDERS names, because the dev pod's two senders take
//! the address from `infra/dev/sor-url` in the checkout rather than from
//! their environment. A block scalar is text this reader skips (the
//! manifest-shell lint executes those), so a host spelled only inside a
//! script is not seen here; every sender today names it in `env`.
//!
//! WHAT HOLDING IS. The container mounts a volume whose Secret is
//! `boss-machine-token`; the mount is `readOnly`, the volume `optional`
//! (an unfilled Secret is an empty directory, never a pod stuck in
//! ContainerCreating), the container names `BOSS_MACHINE_TOKEN_HOSTS`
//! (or it reaches its Service names unstamped), and the reader is the
//! boss image's: the container runs that image, or an init container
//! running it copies its helpers into a volume the container mounts
//! read-only — the estate observer's shape, the only way a reader gets
//! into a pod that does not run the image. The dev pod's two holders run
//! the checkout's own copy and are named for it.
//!
//! WHAT IT ALSO HOLDS, because the grants of 2026-10-07 were conditional:
//!   * the dev pod: the sidecar's mount is the sidecar's — the `dev`
//!     container, where agents run, mounts exactly what it mounted
//!     before, the token at the doors' directory and nowhere else;
//!   * the playground crawl: the container that runs bun and Playwright
//!     has no token, no reader, no helper and no address of a gated
//!     port; it shares two directories with the `record` container and
//!     can write only one of them.
//!
//! tree-wide pin — it reads every manifest under infra/cluster/manifests,
//! which no changed-file map attributes to this crate, so every scoped
//! gate runs it (`tree_wide_pins` in infra/gate.sh).

use boss_testing::rbac::{self, Node, Value, yaml_bool};
use boss_testing::repo_root;
use std::collections::BTreeMap;

const MANIFESTS: &str = "infra/cluster/manifests";
const SECRET: &str = "boss-machine-token";
const ESTATE: &str = "infra/estate/estate.toml";
const SOR_URL_FILE: &str = "infra/dev/sor-url";
const DOORS_DIR_FILE: &str = "infra/dev/machine-token-dir";
const DEV: &str = "boss-dev.yaml";
const CRAWL: &str = "boss-playground-crawl.yaml";

/// Senders whose address is not in their environment: (file, container,
/// where it comes from).
const CHECKOUT_SENDERS: &[(&str, &str, &str)] = &[
    (
        DEV,
        "dev",
        "the doors (infra/dev/boss-api, the boss shim) read infra/dev/sor-url",
    ),
    (
        DEV,
        "reclaim",
        "infra/cluster/dev-scratch-reclaim.sh reads infra/dev/sor-url beside its checkout",
    ),
];

/// Holders whose reader is the checkout's copy, not the boss image's:
/// (file, container, why).
const READER_FROM_THE_CHECKOUT: &[(&str, &str, &str)] = &[
    (
        DEV,
        "dev",
        "the doors source infra/lib/secret-header.sh from the checkout they are symlinked into",
    ),
    (
        DEV,
        "reclaim",
        "the sidecar runs the checkout's dev-scratch-reclaim.sh, whose wrap and step source \
         infra/lib/secret-header.sh from beside themselves; the same volume the dev container \
         already holds the token over (David, design-doc c8502e17 `reclaim-sidecar`)",
    ),
];

/// Senders that hold NO token, and why: (file, container, reason). A
/// row here is a caller the enforce flip refuses, by decision.
const NOT_HOLDERS: &[(&str, &str, &str)] = &[];

struct Container<'a> {
    file: &'a str,
    owner: String,
    name: String,
    init: bool,
    node: &'a Node,
    pod: &'a Node,
}

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

fn items<'a>(node: &'a Node, key: &str) -> Vec<&'a Node> {
    match node.get(key).map(|n| &n.value) {
        Some(Value::Seq(items)) => items.iter().collect(),
        _ => Vec::new(),
    }
}

fn text<'a>(node: &'a Node, key: &str) -> &'a str {
    node.get(key).and_then(Node::str).unwrap_or("")
}

/// Every scalar string anywhere under a node — values and sequence
/// items alike, so an argv word counts as much as an env value.
fn strings<'a>(node: &'a Node, out: &mut Vec<&'a str>) {
    match &node.value {
        Value::Map(entries) => entries.iter().for_each(|(_, n)| strings(n, out)),
        Value::Seq(items) => items.iter().for_each(|n| strings(n, out)),
        Value::Str(s) => out.push(s),
        Value::Text | Value::Null => {}
    }
}

fn parsed(files: &BTreeMap<String, String>) -> Vec<(String, Vec<rbac::Object>)> {
    files
        .iter()
        .map(|(name, text)| {
            let objects = rbac::read_stream(name, text).unwrap_or_else(|e| {
                panic!("{name} does not parse, and an unread pod is not a pod with no mount: {e}")
            });
            (name.clone(), objects)
        })
        .collect()
}

fn containers(parsed: &[(String, Vec<rbac::Object>)]) -> Vec<Container<'_>> {
    let mut out = Vec::new();
    for (file, objects) in parsed {
        for object in objects {
            let Some(pod) = object.pod_spec() else {
                continue;
            };
            for (key, init) in [("initContainers", true), ("containers", false)] {
                for node in items(pod, key) {
                    out.push(Container {
                        file,
                        owner: format!("{} {}", object.kind, object.name),
                        name: text(node, "name").to_string(),
                        init,
                        node,
                        pod,
                    });
                }
            }
        }
    }
    out
}

/// The hosts the machine door fronts: every `*-internal` Service of the
/// source instance's namespace, as a pod spells it.
fn gated_hosts(parsed: &[(String, Vec<rbac::Object>)]) -> Vec<String> {
    let hosts: Vec<String> = parsed
        .iter()
        .flat_map(|(_, objects)| objects)
        .filter(|o| o.kind == "Service" && o.name.ends_with("-internal"))
        .map(|o| {
            format!(
                "{}.{}.svc",
                o.name,
                o.namespace.as_deref().unwrap_or("default")
            )
        })
        .collect();
    assert!(
        hosts.iter().any(|h| h == "boss-jobs-internal.boss.svc"),
        "the scan found no boss-jobs-internal Service — it has stopped seeing the door, and a \
         pin that sees no door passes every pod: {hosts:?}"
    );
    hosts
}

fn boss_image() -> String {
    let estate = std::fs::read_to_string(repo_root().join(ESTATE)).expect(ESTATE);
    let registry = estate
        .lines()
        .find_map(|l| l.strip_prefix("forge_registry = \""))
        .and_then(|l| l.strip_suffix('"'))
        .unwrap_or_else(|| panic!("{ESTATE} names no forge_registry"));
    format!("{registry}/david/boss:")
}

impl Container<'_> {
    fn at(&self) -> String {
        format!("{} ({}) container `{}`", self.file, self.owner, self.name)
    }

    fn mounts(&self) -> Vec<&Node> {
        items(self.node, "volumeMounts")
    }

    fn volume(&self, name: &str) -> Option<&Node> {
        items(self.pod, "volumes")
            .into_iter()
            .find(|v| text(v, "name") == name)
    }

    /// The mounts of this container whose volume is the token's Secret.
    fn token_mounts(&self) -> Vec<&Node> {
        self.mounts()
            .into_iter()
            .filter(|m| {
                self.volume(text(m, "name"))
                    .and_then(|v| v.path(&["secret", "secretName"]))
                    .and_then(Node::str)
                    == Some(SECRET)
            })
            .collect()
    }

    fn env_names(&self) -> Vec<&str> {
        items(self.node, "env")
            .into_iter()
            .map(|e| text(e, "name"))
            .collect()
    }

    fn names_a_gated_host(&self, hosts: &[String]) -> bool {
        let mut all = Vec::new();
        strings(self.node, &mut all);
        all.iter()
            .any(|s| hosts.iter().any(|h| s.contains(h.as_str())))
    }

    fn listed(&self, roster: &[(&str, &str, &str)]) -> bool {
        roster
            .iter()
            .any(|(f, c, _)| *f == self.file && *c == self.name)
    }
}

/// The findings of the main rule, one line each.
fn findings(files: &BTreeMap<String, String>, not_holders: &[(&str, &str, &str)]) -> Vec<String> {
    let parsed = parsed(files);
    let hosts = gated_hosts(&parsed);
    let boss = boss_image();
    let all = containers(&parsed);
    let mut out = Vec::new();
    let mut senders = 0;
    for c in &all {
        let sends = c.names_a_gated_host(&hosts) || c.listed(CHECKOUT_SENDERS);
        let held = c.token_mounts();
        if !sends {
            continue;
        }
        senders += 1;
        if held.is_empty() {
            if !c.listed(not_holders) {
                out.push(format!(
                    "{}: sends to a port the machine gate guards and mounts no {SECRET} — every \
                     request it sends is one the gate would refuse. Mount the Secret read-only \
                     and optional and take the reader from the boss image (the shape of \
                     boss-estate-observe.yaml), or name it in NOT_HOLDERS with the reason it \
                     must not hold the token. A new mount is a grant: it needs a decision",
                    c.at()
                ));
            }
            continue;
        }
        if c.listed(not_holders) {
            out.push(format!(
                "{}: NOT_HOLDERS says it must not hold the token, and it mounts it",
                c.at()
            ));
        }
        for m in &held {
            if m.get("readOnly").and_then(yaml_bool) != Some(true) {
                out.push(format!(
                    "{}: the token mount at {} is not readOnly",
                    c.at(),
                    text(m, "mountPath")
                ));
            }
            if m.get("subPath").is_some() {
                out.push(format!(
                    "{}: the token is mounted by subPath, which a rotation never reaches",
                    c.at()
                ));
            }
            let volume = c
                .volume(text(m, "name"))
                .expect("the volume it was matched by");
            if volume.path(&["secret", "optional"]).and_then(yaml_bool) != Some(true) {
                out.push(format!(
                    "{}: Secret {SECRET} is not optional — an unfilled Secret holds the pod in \
                     ContainerCreating and the sender never runs",
                    c.at()
                ));
            }
        }
        // The dev container's doors name the host themselves, to what
        // they run and nothing else (infra/dev/boss reads sor-url): an
        // env var here would reach every process on the pod.
        let doors_name_it = c.file == DEV && c.name == "dev";
        if !doors_name_it && !c.env_names().contains(&"BOSS_MACHINE_TOKEN_HOSTS") {
            out.push(format!(
                "{}: holds the token and names no BOSS_MACHINE_TOKEN_HOSTS — the reader sends \
                 the token to loopback and to the hosts that names, so this container reaches \
                 its Service names unstamped",
                c.at()
            ));
        }
        let image = text(c.node, "image");
        let from_an_init = all.iter().any(|i| {
            i.init
                && std::ptr::eq(i.pod, c.pod)
                && text(i.node, "image").starts_with(&boss)
                && i.mounts().iter().any(|im| {
                    c.mounts().iter().any(|cm| {
                        text(cm, "name") == text(im, "name")
                            && cm.get("readOnly").and_then(yaml_bool) == Some(true)
                    })
                })
        });
        if !(image.starts_with(&boss) || from_an_init || c.listed(READER_FROM_THE_CHECKOUT)) {
            out.push(format!(
                "{}: holds the token and has no reader from the boss image — it runs {image}, \
                 and no init container running the boss image copies its helpers into a volume \
                 this container mounts read-only",
                c.at()
            ));
        }
    }
    for (file, container, why) in not_holders {
        match all.iter().find(|c| c.file == *file && c.name == *container) {
            None => out.push(format!(
                "NOT_HOLDERS names {file} container `{container}`, which is gone ({why}) — drop the row"
            )),
            Some(c) if !(c.names_a_gated_host(&hosts) || c.listed(CHECKOUT_SENDERS)) => {
                // Printed, not refused: the car that stops a sender
                // sending and the car that listed it may be two cars,
                // and a pin red on the assembled tree for that would
                // punish the fix.
                println!("stale: NOT_HOLDERS names {file} `{container}`, which sends to no gated host now ({why})");
            }
            Some(_) => {}
        }
    }
    assert!(
        senders >= 10,
        "the scan found {senders} sender(s) — the chores alone are more; it has stopped seeing \
         the tree"
    );
    out
}

#[test]
fn every_in_cluster_sender_holds_the_machine_token_or_is_named() {
    let found = findings(&manifests(), NOT_HOLDERS);
    assert!(
        found.is_empty(),
        "in-cluster senders the machine gate would refuse, or holders that hold it wrongly:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn the_dev_pods_senders_reach_a_gated_host_through_the_checkout() {
    let url = std::fs::read_to_string(repo_root().join(SOR_URL_FILE)).expect(SOR_URL_FILE);
    let files = manifests();
    let hosts = gated_hosts(&parsed(&files));
    assert!(
        hosts.iter().any(|h| url.contains(h.as_str())),
        "{SOR_URL_FILE} ({}) names none of the machine door's Services {hosts:?} — \
         CHECKOUT_SENDERS lists the dev pod's containers as senders because of that file",
        url.trim()
    );
}

// ---- the rule refuses what it should (mutants, run in-process) ------------

fn edited(file: &str, from: &str, to: &str) -> BTreeMap<String, String> {
    let mut files = manifests();
    let text = files.get_mut(file).unwrap_or_else(|| panic!("{file}"));
    assert!(
        text.contains(from),
        "{file} no longer holds `{from}` — the mutant below would test nothing"
    );
    *text = text.replacen(from, to, 1);
    files
}

fn refused(files: &BTreeMap<String, String>, needle: &str) {
    let found = findings(files, NOT_HOLDERS);
    assert!(
        found.iter().any(|f| f.contains(needle)),
        "expected a finding holding `{needle}`, got: {found:#?}"
    );
}

#[test]
fn a_sender_without_the_mount_is_refused_by_name() {
    // The recovery sheet as it was before this car: the address, no mount.
    let files = edited(
        "boss-recovery-sheet.yaml",
        "                - {name: machine-token, mountPath: /etc/boss/machine-token, readOnly: true}\n",
        "",
    );
    refused(
        &files,
        "boss-recovery-sheet.yaml (CronJob boss-recovery-sheet) container `check`: sends to a port",
    );
    // The sidecar, which names no address in its env at all.
    let files = edited(
        DEV,
        "            - {name: machine-token, mountPath: /etc/boss/machine-token, readOnly: true}\n",
        "",
    );
    refused(
        &files,
        "boss-dev.yaml (Deployment boss-dev) container `reclaim`: sends to a port",
    );
}

#[test]
fn a_holder_that_holds_it_wrongly_is_refused() {
    let mount = "                - {name: machine-token, mountPath: /etc/boss/machine-token, readOnly: true}\n";
    let files = edited(
        "boss-recovery-sheet.yaml",
        mount,
        "                - {name: machine-token, mountPath: /etc/boss/machine-token}\n",
    );
    refused(&files, "is not readOnly");
    let files = edited(
        "boss-recovery-sheet.yaml",
        mount,
        "                - {name: machine-token, mountPath: /etc/boss/machine-token, readOnly: no}\n",
    );
    refused(&files, "is not readOnly");
    let files = edited(
        "boss-recovery-sheet.yaml",
        "secret: {secretName: boss-machine-token, optional: true,",
        "secret: {secretName: boss-machine-token,",
    );
    refused(&files, "is not optional");
    let files = edited(
        "boss-recovery-sheet.yaml",
        "                - name: BOSS_MACHINE_TOKEN_HOSTS\n",
        "                - name: BOSS_SOMETHING_ELSE\n",
    );
    refused(&files, "names no BOSS_MACHINE_TOKEN_HOSTS");
    // The reader's init container on another image: nothing puts the
    // boss image's reader in the pod.
    let files = edited(
        "boss-recovery-sheet.yaml",
        "            - name: tools\n              image: 10.20.0.15:3000/david/boss:latest\n",
        "            - name: tools\n              image: 10.20.0.15:3000/david/boss-ci:rust1.96\n",
    );
    refused(&files, "has no reader from the boss image");
}

#[test]
fn a_row_for_a_holder_or_a_container_that_is_gone_is_refused() {
    let files = manifests();
    let found = findings(
        &files,
        &[("boss-recovery-sheet.yaml", "check", "a fixture row")],
    );
    assert!(
        found
            .iter()
            .any(|f| f.contains("NOT_HOLDERS says it must not hold the token")),
        "{found:#?}"
    );
    let found = findings(
        &files,
        &[("boss-recovery-sheet.yaml", "gone", "a fixture row")],
    );
    assert!(
        found.iter().any(|f| f.contains("which is gone")),
        "{found:#?}"
    );
}

// ---- the dev pod: the sidecar's mount is the sidecar's ---------------------

/// What the `dev` container mounts — the list as it stood before the
/// sidecar was granted the token (origin/main a5575ef9, 2026-10-07).
/// A mount added here is a grant to the container agents run in.
const DEV_CONTAINER_MOUNTS: &[&str] = &[
    "work",
    "scratch",
    "forge-token",
    "forge-write-token",
    "ssh-authorized",
    "access-ssh-ca",
    "machine-token",
];

fn dev_findings(dev_yaml: &str) -> Vec<String> {
    let files = BTreeMap::from([(DEV.to_string(), dev_yaml.to_string())]);
    let parsed = parsed(&files);
    let all = containers(&parsed);
    let doors = std::fs::read_to_string(repo_root().join(DOORS_DIR_FILE))
        .expect(DOORS_DIR_FILE)
        .trim()
        .to_string();
    let default_dir = boss_core::machine_token::DEFAULT_TOKEN_DIR;
    let mut out = Vec::new();
    let find = |name: &str| {
        all.iter()
            .find(|c| c.name == name && !c.init)
            .unwrap_or_else(|| panic!("{DEV} has no `{name}` container"))
    };
    let dev = find("dev");
    let reclaim = find("reclaim");
    let names: Vec<&str> = dev.mounts().iter().map(|m| text(m, "name")).collect();
    if names != DEV_CONTAINER_MOUNTS {
        out.push(format!(
            "the dev container mounts {names:?}, not {DEV_CONTAINER_MOUNTS:?} — the container \
             agents run in gained or lost a mount"
        ));
    }
    let paths = |c: &Container| -> Vec<String> {
        c.token_mounts()
            .iter()
            .map(|m| text(m, "mountPath").to_string())
            .collect()
    };
    if paths(dev) != [doors.clone()] {
        out.push(format!(
            "the dev container holds the token at {:?}, not at the doors' directory {doors} alone",
            paths(dev)
        ));
    }
    if paths(reclaim) != [default_dir.to_string()] {
        out.push(format!(
            "the reclaim sidecar holds the token at {:?}, not once at {default_dir}, where the \
             reader looks when nothing names a directory",
            paths(reclaim)
        ));
    }
    // The sidecar's header file lives in memory, and in a directory the
    // dev container does not mount: a header file is the token.
    let runtime = items(reclaim.node, "env")
        .into_iter()
        .find(|e| text(e, "name") == "RUNTIME_DIRECTORY")
        .map(|e| text(e, "value").to_string())
        .unwrap_or_default();
    let header_mount = reclaim
        .mounts()
        .into_iter()
        .find(|m| !runtime.is_empty() && text(m, "mountPath") == runtime);
    match header_mount {
        None => out.push(format!(
            "the reclaim sidecar's RUNTIME_DIRECTORY ({runtime:?}) is not a mount of its own — \
             the reader would write the header file under /tmp, which outlives a killed pass"
        )),
        Some(m) => {
            let volume = text(m, "name");
            let memory = reclaim
                .volume(volume)
                .and_then(|v| v.path(&["emptyDir", "medium"]))
                .and_then(Node::str)
                == Some("Memory");
            if !memory {
                out.push(format!(
                    "the reclaim sidecar's header directory (volume `{volume}`) is not a memory emptyDir"
                ));
            }
            for other in all.iter().filter(|c| c.name != "reclaim") {
                if other.mounts().iter().any(|om| text(om, "name") == volume) {
                    out.push(format!(
                        "container `{}` mounts the reclaim sidecar's header directory `{volume}`",
                        other.name
                    ));
                }
            }
        }
    }
    // No container of the pod is told the token's directory by name:
    // every process would inherit it, builders' tests included.
    for c in &all {
        if c.env_names().contains(&"BOSS_MACHINE_TOKEN_DIR") {
            out.push(format!(
                "container `{}` names BOSS_MACHINE_TOKEN_DIR in its environment",
                c.name
            ));
        }
    }
    for c in all
        .iter()
        .filter(|c| !["dev", "reclaim"].contains(&c.name.as_str()))
    {
        if !c.token_mounts().is_empty() {
            out.push(format!("container `{}` mounts the token", c.name));
        }
    }
    out
}

#[test]
fn the_sidecars_token_is_the_sidecars_and_the_dev_container_gains_nothing() {
    let dev = &manifests()[DEV];
    let found = dev_findings(dev);
    assert!(found.is_empty(), "{DEV}:\n  {}", found.join("\n  "));
}

#[test]
fn a_mount_added_to_the_dev_container_is_refused() {
    let dev = manifests()[DEV].clone();
    let doors_mount = "            - {name: machine-token, mountPath: /etc/boss-doors/machine-token, readOnly: true}\n";
    assert!(
        dev.contains(doors_mount),
        "the mutants below would test nothing"
    );
    // A second token mount on the dev container, at the default directory.
    let m = dev.replacen(
        doors_mount,
        &format!("{doors_mount}            - {{name: machine-token, mountPath: /etc/boss/machine-token, readOnly: true}}\n"),
        1,
    );
    let found = dev_findings(&m);
    assert!(
        found.iter().any(|f| f.contains("gained or lost a mount")),
        "{found:#?}"
    );
    assert!(
        found.iter().any(|f| f.contains("the doors' directory")),
        "{found:#?}"
    );
    // The sidecar's header directory, mounted into the dev container.
    let m = dev.replacen(
        doors_mount,
        &format!(
            "{doors_mount}            - {{name: reclaim-secret-header, mountPath: /run/reclaim}}\n"
        ),
        1,
    );
    let found = dev_findings(&m);
    assert!(
        found
            .iter()
            .any(|f| f.contains("mounts the reclaim sidecar's header directory")),
        "{found:#?}"
    );
    // The sidecar's mount removed: it sends bare again.
    let m = dev.replacen(
        "            - {name: machine-token, mountPath: /etc/boss/machine-token, readOnly: true}\n",
        "",
        1,
    );
    let found = dev_findings(&m);
    assert!(
        found
            .iter()
            .any(|f| f.contains("the reclaim sidecar holds the token at []")),
        "{found:#?}"
    );
    // The directory named in an environment.
    let m = dev.replacen(
        "            - {name: REPO_DIR, value: /work/boss}\n",
        "            - {name: REPO_DIR, value: /work/boss}\n            - {name: BOSS_MACHINE_TOKEN_DIR, value: /etc/boss/machine-token}\n",
        1,
    );
    let found = dev_findings(&m);
    assert!(
        found
            .iter()
            .any(|f| f.contains("names BOSS_MACHINE_TOKEN_DIR")),
        "{found:#?}"
    );
}

// ---- the crawl: the browser's container never holds it ---------------------

/// What the `crawl` container may mount, and whether it may write it.
const CRAWL_MOUNTS: &[(&str, bool)] = &[
    ("work", true),
    ("forge-token", false),
    ("result", true),
    ("go", false),
];

fn crawl_findings(crawl_yaml: &str) -> Vec<String> {
    let mut files = manifests();
    files.insert(CRAWL.to_string(), crawl_yaml.to_string());
    let parsed = parsed(&files);
    let hosts = gated_hosts(&parsed);
    let boss = boss_image();
    let all: Vec<Container> = containers(&parsed)
        .into_iter()
        .filter(|c| c.file == CRAWL)
        .collect();
    let mut out = Vec::new();
    let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
    if names != ["crawl", "record"] {
        out.push(format!(
            "the pod's containers are {names:?}, not [crawl, record] — a third container, or an \
             init container, is a third party to the hand-over"
        ));
        return out;
    }
    let (crawl, record) = (&all[0], &all[1]);
    if !crawl.token_mounts().is_empty() {
        out.push("the crawl container mounts the machine token".to_string());
    }
    if crawl.names_a_gated_host(&hosts) {
        out.push(
            "the crawl container names a host the machine gate guards — it has somewhere to \
             send a request the gate would refuse"
                .to_string(),
        );
    }
    for forbidden in [
        "BOSS_JOBS_URL",
        "BOSS_MACHINE_TOKEN_HOSTS",
        "BOSS_MACHINE_TOKEN_DIR",
        "RUNTIME_DIRECTORY",
        "BOSS_API_CURL",
    ] {
        if crawl.env_names().contains(&forbidden) {
            out.push(format!(
                "the crawl container's environment names {forbidden}"
            ));
        }
    }
    if text(crawl.node, "image").starts_with(&boss) {
        out.push(
            "the crawl container runs the boss image, which carries the chore helpers".to_string(),
        );
    }
    let got: Vec<(&str, bool)> = crawl
        .mounts()
        .iter()
        .map(|m| {
            (
                text(m, "name"),
                m.get("readOnly").and_then(yaml_bool) != Some(true),
            )
        })
        .collect();
    if got != CRAWL_MOUNTS {
        out.push(format!(
            "the crawl container mounts (volume, writable) {got:?}, not {CRAWL_MOUNTS:?}"
        ));
    }
    // The record container: the boss image, the token, and the two
    // shared directories the other way round.
    if !text(record.node, "image").starts_with(&boss) {
        out.push("the record container does not run the boss image".to_string());
    }
    if record.token_mounts().len() != 1 {
        out.push("the record container does not mount the machine token exactly once".to_string());
    }
    let record_mount = |name: &str| {
        record
            .mounts()
            .into_iter()
            .find(|m| text(m, "name") == name)
            .map(|m| m.get("readOnly").and_then(yaml_bool) == Some(true))
    };
    if record_mount("result") != Some(true) {
        out.push(
            "the record container does not mount `result` read-only — what the crawl leaves is \
             read, never written back"
                .to_string(),
        );
    }
    if record_mount("go") != Some(false) {
        out.push("the record container cannot write the `go` signal".to_string());
    }
    for theirs in ["work", "forge-token"] {
        if record_mount(theirs).is_some() {
            out.push(format!(
                "the record container mounts `{theirs}` — the clone and its credential are the crawl's"
            ));
        }
    }
    // The shared directories are bounded: what a container can fill is
    // what it can take from the node.
    for shared in ["result", "go"] {
        let bounded = crawl
            .volume(shared)
            .and_then(|v| v.path(&["emptyDir", "sizeLimit"]))
            .is_some();
        if !bounded {
            out.push(format!(
                "the `{shared}` volume is not an emptyDir with a sizeLimit"
            ));
        }
    }
    out
}

#[test]
fn the_crawl_container_holds_no_token_and_names_no_gated_port() {
    let found = crawl_findings(&manifests()[CRAWL]);
    assert!(found.is_empty(), "{CRAWL}:\n  {}", found.join("\n  "));
}

#[test]
fn a_crawl_container_given_the_token_or_the_address_is_refused() {
    let crawl = manifests()[CRAWL].clone();
    let result_mount = "                - {name: result, mountPath: /result}\n";
    assert!(
        crawl.contains(result_mount),
        "the mutants below would test nothing"
    );
    let m = crawl.replacen(
        result_mount,
        &format!("{result_mount}                - {{name: machine-token, mountPath: /etc/boss/machine-token, readOnly: true}}\n"),
        1,
    );
    let found = crawl_findings(&m);
    assert!(
        found
            .iter()
            .any(|f| f.contains("the crawl container mounts the machine token")),
        "{found:#?}"
    );
    let forge =
        "                - {name: FORGE_URL, value: http://10.20.0.15:3000/david/boss.git}\n";
    assert!(
        crawl.contains(forge),
        "the mutants below would test nothing"
    );
    let m = crawl.replacen(
        forge,
        &format!("{forge}                - {{name: WHERE, value: \"http://boss-jobs-internal.boss.svc.cluster.local:7900\"}}\n"),
        1,
    );
    let found = crawl_findings(&m);
    assert!(
        found
            .iter()
            .any(|f| f.contains("names a host the machine gate guards")),
        "{found:#?}"
    );
    let m = crawl.replacen(
        "                - {name: go, mountPath: /go, readOnly: true}\n",
        "                - {name: go, mountPath: /go}\n",
        1,
    );
    let found = crawl_findings(&m);
    assert!(
        found
            .iter()
            .any(|f| f.contains("the crawl container mounts (volume, writable)")),
        "{found:#?}"
    );
    let m = crawl.replacen(
        "                - {name: result, mountPath: /result, readOnly: true}\n",
        "                - {name: result, mountPath: /result}\n",
        1,
    );
    let found = crawl_findings(&m);
    assert!(
        found
            .iter()
            .any(|f| f.contains("does not mount `result` read-only")),
        "{found:#?}"
    );
}
