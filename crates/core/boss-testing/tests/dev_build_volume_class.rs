//! WHERE A DEV-BUILD WORKER'S VOLUME MAY LIVE (backlog e14a472d, option C
//! decided by David 2026-10-29 on d9b39685).
//!
//! Each dev-build worker gets a one-replica Longhorn volume, sized by
//! `WORKER_VOLUME_GIB` in infra/dev/dev-build.py (100Gi when this file was
//! written; backlog b3ad4219 resized it to what a builder measurably
//! holds). Until
//! this file the class it was cut from, `longhorn-dev-disposable`, bound
//! `Immediate` and carried no placement rule, so Longhorn placed the one
//! replica the moment the claim existed — before the pod, which is
//! required off the control planes, was even scheduled — on any disk with
//! room under its floor. On 2026-10-27 that was cp-3, whose system disk
//! also carries etcd and the PostgreSQL replicas, with 150 GB placeable.
//!
//! The fix is a NEW class (a StorageClass's binding mode and parameters
//! are immutable, and `kubectl apply` of an edited one fails the
//! converge), `longhorn-dev-build`, which binds on its first consumer and
//! names a Longhorn node tag in `nodeSelector`, so a replica can only be
//! placed on a node carrying it. Longhorn has no negative selector, so
//! the tag is positive: it is declared on every Talos WORKER as the
//! `node.longhorn.io/default-node-tags` annotation, which Longhorn copies
//! onto a node whose tag list is empty (every node's, read 2026-10-29),
//! and never on a control plane — nor, since 2026-11-11, on the data node
//! (boss.dev/purpose=database, design 3c475a19; backlog 1011f72f). These
//! tests are the equality the three places owe each other (CLAUDE.md
//! §9a): the class's selector, the workers' declared tag, and the absence
//! of it on every control plane and on the data node.
//!
//! Read, not run: a class and a declaration are text; what the cluster
//! did with them is read back by node-converge and by dev-build's own
//! storage budget, which lists Longhorn's nodes before every start.

use boss_testing::repo_root;
use serde::Deserialize;
use serde_json::Value;

const CLASS: &str = "longhorn-dev-build";
const TAG_ANNOTATION: &str = "node.longhorn.io/default-node-tags";

fn documents(path: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(repo_root().join(path)).unwrap();
    yaml_serde::Deserializer::from_str(&text)
        .map(|document| Value::deserialize(document).unwrap())
        .collect()
}

fn the_class() -> Value {
    documents("infra/cluster/manifests/boss-dev.yaml")
        .into_iter()
        .find(|d| d["kind"] == "StorageClass" && d["metadata"]["name"] == CLASS)
        .unwrap_or_else(|| panic!("boss-dev.yaml declares StorageClass {CLASS}"))
}

/// The tags `nodeSelector` names, as Longhorn reads it: comma-separated.
fn selected_tags(class: &Value) -> Vec<String> {
    class["parameters"]["nodeSelector"]
        .as_str()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// The purpose of the data node (design 3c475a19: the Dell, w-3, carries
/// the system of record's PostgreSQL and durable Longhorn on its Microns,
/// and is "never a build node"). Rebuildable build storage belongs on
/// the other workers' disks, so this purpose is the one a worker declares
/// to keep the dev-build tag off itself (backlog 1011f72f).
const DATA_PURPOSE: &str = "database";

/// The `boss.dev/purpose=<value>` labels an estate row's notes name.
/// The estate row is where the tree declares a worker's purpose (w-1's
/// `build` is cited from there by boss-node-maintenance.yaml); there is
/// no structured key for it, so the label is read as the exact token the
/// cluster's node label spells, never as a phrase.
fn purposes(notes: &str) -> Vec<String> {
    notes
        .split("boss.dev/purpose=")
        .skip(1)
        .map(|rest| {
            rest.chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                .collect::<String>()
        })
        .collect()
}

/// Every Talos node the estate declares: its id, its role, and — for a
/// worker — the one purpose its row declares.
fn talos_nodes() -> Vec<(String, String, Option<String>)> {
    let path = repo_root().join("infra/estate/estate.toml");
    let estate: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let nodes: Vec<(String, String, Option<String>)> = estate["node"]
        .as_array()
        .expect("estate.toml declares [[node]] tables")
        .iter()
        .filter_map(|n| {
            let role = n["role"].as_str()?;
            let id = n["id"].as_str().unwrap().to_string();
            role.starts_with("talos-").then(|| {
                let purpose = (role == "talos-worker").then(|| {
                    // A worker whose notes lost the label would silently
                    // fall out of the exemption below; refuse instead.
                    let found = purposes(n["notes"].as_str().unwrap_or(""));
                    assert_eq!(
                        found.len(),
                        1,
                        "worker {id}'s estate notes name exactly one boss.dev/purpose label: {found:?}"
                    );
                    found[0].clone()
                });
                (id, role.to_string(), purpose)
            })
        })
        .collect();
    // Control: a reader that found nothing would pass every loop below.
    assert!(
        nodes.iter().any(|(_, r, _)| r == "talos-worker")
            && nodes.iter().any(|(_, r, _)| r == "talos-control-plane"),
        "the estate declares Talos workers and control planes: {nodes:?}"
    );
    nodes
}

/// The Longhorn tags a node's Talos declaration hands Longhorn, if any.
fn declared_tags(node: &str) -> Option<Vec<String>> {
    let path = format!("infra/cluster/talos/patches/{node}.yaml");
    let machine = documents(&path)
        .into_iter()
        .find(|d| d.get("machine").is_some())
        .unwrap_or_else(|| panic!("{path} carries a machine fragment"));
    let value = machine["machine"]["nodeAnnotations"][TAG_ANNOTATION].as_str()?;
    let tags: Vec<String> = serde_json::from_str(value).unwrap_or_else(|e| {
        panic!("{path}: {TAG_ANNOTATION} must be a JSON list of strings, as Longhorn reads it: {value:?} ({e})")
    });
    Some(tags)
}

#[test]
fn the_dev_build_class_binds_on_its_pod_and_places_only_on_tagged_nodes() {
    let class = the_class();
    assert_eq!(class["provisioner"], "driver.longhorn.io", "{class}");
    // The claim waits for the pod, and the pod is required off the
    // control planes (dev-build.py's nodeAffinity), so the volume is
    // never made for a node the pod cannot run on.
    assert_eq!(
        class["volumeBindingMode"], "WaitForFirstConsumer",
        "{class}"
    );
    assert_eq!(class["reclaimPolicy"], "Delete", "{class}");
    let p = &class["parameters"];
    assert_eq!(
        p["numberOfReplicas"], "1",
        "a worker volume is disposable: {class}"
    );
    assert_eq!(p["dataLocality"], "best-effort", "{class}");
    // The placement rule itself: one positive tag, and no disk selector
    // that would narrow it to disks nothing tags.
    assert_eq!(
        selected_tags(&class),
        vec!["dev-build".to_string()],
        "{class}"
    );
    assert!(p.get("diskSelector").is_none(), "{class}");
}

/// Where build storage may be placed: every worker but the data node.
/// Until 2026-11-11 this pin demanded the tag of EVERY worker, so the
/// data node's first declaration carried it, and a dev-build replica
/// could have landed beside the system of record — the class has no
/// diskSelector and best-effort locality, so the replica's node need not
/// be its pod's (review 7178449c of car f0add3fb, backlog 1011f72f).
#[test]
fn build_storage_is_declared_on_every_worker_but_the_data_node_and_no_control_plane() {
    let tags = selected_tags(&the_class());
    let nodes = talos_nodes();
    // Control: the exemption is exercised by a node the estate declares,
    // and the class still has somewhere to place.
    assert!(
        nodes
            .iter()
            .any(|(_, _, p)| p.as_deref() == Some(DATA_PURPOSE)),
        "the estate declares a {DATA_PURPOSE} worker: {nodes:?}"
    );
    assert!(
        nodes
            .iter()
            .any(|(_, r, p)| r == "talos-worker" && p.as_deref() != Some(DATA_PURPOSE)),
        "a worker is left for the class to place on: {nodes:?}"
    );
    for (node, role, purpose) in nodes {
        let declared = declared_tags(&node);
        if role == "talos-control-plane" {
            // An untagged control plane is what keeps the replica off
            // the disk that carries etcd and the database.
            assert!(
                declared.is_none(),
                "{node} is a control plane and must hand Longhorn no tag: {declared:?}"
            );
        } else if purpose.as_deref() == Some(DATA_PURPOSE) {
            // The regression: the data node's disks hold the system of
            // record and durable volumes, never a disposable replica.
            assert!(
                declared.is_none(),
                "{node} is the data node (boss.dev/purpose={DATA_PURPOSE}, design 3c475a19) and must hand Longhorn no tag: {declared:?}"
            );
        } else {
            let declared = declared
                .unwrap_or_else(|| panic!("{node} is a worker and declares no {TAG_ANNOTATION}"));
            for t in &tags {
                assert!(
                    declared.contains(t),
                    "{node} declares {declared:?}, missing the class's tag {t}"
                );
            }
        }
    }
}

/// THE SCRATCH DISK (design 3c475a19 D2/D3, section 4 car 2; backlog
/// 46328c3a). On 2026-11-12 two builder runs were refused by the worker
/// door because every worker volume competes with durable replicas for
/// w-1's 980 PRO default disk, while w-1's Kingston SNV3S1000G sat
/// fitted and idle. The Kingston becomes a Talos user volume `scratch`,
/// add-longhorn-disk makes it Longhorn disk `u-scratch`, the disk-tag
/// door tags it `scratch`, and worker volumes are cut from a class whose
/// `diskSelector` names that tag, so they land there and nowhere else.
///
/// A NEW class, as `longhorn-dev-build` was a new class beside
/// `longhorn-dev-disposable` (the test below): a StorageClass's
/// parameters are immutable and `kubectl apply` of an edited one fails
/// the converge. It keeps the build-node tag beside the disk tag, so it
/// can never place on a node the old class could not.
const SCRATCH_CLASS: &str = "longhorn-dev-scratch";
const SCRATCH_TAG: &str = "scratch";

fn class_named(name: &str, file: &str) -> Value {
    documents(file)
        .into_iter()
        .find(|d| d["kind"] == "StorageClass" && d["metadata"]["name"] == name)
        .unwrap_or_else(|| panic!("{file} declares StorageClass {name}"))
}

/// The tags a class's `diskSelector` names, as Longhorn reads it.
fn disk_tags(class: &Value) -> Vec<String> {
    class["parameters"]["diskSelector"]
        .as_str()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn the_scratch_class_places_only_on_a_scratch_disk_of_a_build_node() {
    let old = the_class();
    let class = class_named(SCRATCH_CLASS, "infra/cluster/manifests/boss-dev.yaml");
    assert_eq!(class["provisioner"], "driver.longhorn.io", "{class}");
    assert_eq!(
        class["volumeBindingMode"], "WaitForFirstConsumer",
        "{class}"
    );
    assert_eq!(class["reclaimPolicy"], "Delete", "{class}");
    let p = &class["parameters"];
    assert_eq!(p["numberOfReplicas"], "1", "disposable: {class}");
    assert_eq!(p["dataLocality"], old["parameters"]["dataLocality"]);
    assert_eq!(
        selected_tags(&class),
        selected_tags(&old),
        "the same build nodes as the class it replaces, never more: {class}"
    );
    assert_eq!(
        disk_tags(&class),
        vec![SCRATCH_TAG.to_string()],
        "only a disk tagged scratch: {class}"
    );

    // The scratch disk exists in the tree: a build node declares the
    // user volume add-longhorn-disk turns into Longhorn disk u-scratch,
    // and that node carries the class's node tag, so the disk is one the
    // class can reach.
    let holders: Vec<String> = talos_nodes()
        .into_iter()
        .map(|(node, _, _)| node)
        .filter(|node| {
            documents(&format!("infra/cluster/talos/patches/{node}.yaml"))
                .iter()
                .any(|d| d["kind"] == "UserVolumeConfig" && d["name"] == SCRATCH_TAG)
        })
        .collect();
    assert_eq!(
        holders,
        vec!["w-1".to_string()],
        "w-1's Kingston is the one scratch volume (design 3c475a19 section 2)"
    );
    let tags = declared_tags("w-1").expect("w-1 declares its Longhorn node tag");
    for t in selected_tags(&class) {
        assert!(tags.contains(&t), "w-1 carries {t}: {tags:?}");
    }
}

/// Durable volumes are placed by tag (design 3c475a19 D2): three
/// replicas, only on disks tagged `durable`, and never on the scratch
/// disk, which nothing durable may use (D3).
#[test]
fn the_durable_class_places_three_replicas_on_durable_disks_only() {
    let class = class_named(
        "longhorn-durable",
        "infra/cluster/manifests/longhorn-durable.yaml",
    );
    assert_eq!(class["provisioner"], "driver.longhorn.io", "{class}");
    let p = &class["parameters"];
    assert_eq!(p["numberOfReplicas"], "3", "{class}");
    assert_eq!(disk_tags(&class), vec!["durable".to_string()], "{class}");
    assert!(
        !disk_tags(&class).iter().any(|t| t == SCRATCH_TAG),
        "{class}"
    );
    assert!(
        p.get("nodeSelector").is_none(),
        "placed by disk, on any node with a durable disk: {class}"
    );
}

#[test]
fn the_old_class_is_left_as_it_was_for_the_volumes_that_hold_it() {
    // The dev pod's /work and the gate's disk still name the old class,
    // and Kubernetes refuses an edit to its binding mode or parameters:
    // the converge would fail on it. It is replaced by name, not edited.
    let old = documents("infra/cluster/manifests/boss-dev.yaml")
        .into_iter()
        .find(|d| d["kind"] == "StorageClass" && d["metadata"]["name"] == "longhorn-dev-disposable")
        .expect("the old class is still declared");
    assert_eq!(old["volumeBindingMode"], "Immediate", "{old}");
    assert!(old["parameters"].get("nodeSelector").is_none(), "{old}");
}

/// dev-build names the class it cuts a worker volume from and the node and
/// disk tags its storage budget counts, each once, as Python literals; all
/// three must be the manifest's, or the budget counts disks the class
/// cannot place on.
fn dev_build_literal(name: &str) -> String {
    let source = std::fs::read_to_string(repo_root().join("infra/dev/dev-build.py")).unwrap();
    let prefix = format!("{name} = \"");
    let lines: Vec<&str> = source
        .lines()
        .filter_map(|l| l.strip_prefix(prefix.as_str()))
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "dev-build.py assigns {name} once: {lines:?}"
    );
    lines[0]
        .strip_suffix('"')
        .unwrap_or_else(|| panic!("{name} is one string literal: {:?}", lines[0]))
        .to_string()
}

/// Step 6 of the scratch-disk sequence (infra/cluster/talos/README.md;
/// backlog 46328c3a): once w-1's Kingston was Longhorn disk `u-scratch`
/// tagged `scratch` (ops-requests 82fc4fdd and 12069869, 2026-11-13),
/// worker volumes are cut from the scratch class, and the budget counts
/// only the disks its `diskSelector` names, so the worker door judges a
/// start against the Kingston's room, not the 980 PRO's.
#[test]
fn dev_build_cuts_its_volumes_from_the_scratch_class_and_counts_its_tags() {
    let class = class_named(SCRATCH_CLASS, "infra/cluster/manifests/boss-dev.yaml");
    assert_eq!(dev_build_literal("WORKER_VOLUME_CLASS"), SCRATCH_CLASS);
    assert_eq!(
        vec![dev_build_literal("WORKER_NODE_TAG")],
        selected_tags(&class),
        "the budget counts the nodes the class selects"
    );
    assert_eq!(
        vec![dev_build_literal("WORKER_DISK_TAG")],
        disk_tags(&class),
        "the budget counts the disks the class selects"
    );
    let source = std::fs::read_to_string(repo_root().join("infra/dev/dev-build.py")).unwrap();
    for old in ["longhorn-dev-disposable", CLASS] {
        assert!(
            !source.contains(&format!("\"storageClassName\": \"{old}\"")),
            "no worker volume is cut from {old}"
        );
    }
}
