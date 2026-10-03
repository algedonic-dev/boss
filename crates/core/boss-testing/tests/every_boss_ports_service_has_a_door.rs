//! Every service boss-ports declares has a door the dev pod can read it
//! through, or is named here as having none, with the reason (backlog
//! `9a440539`, 2026-10-01).
//!
//! WHAT WAS MEASURED. Twice that day the pod's door could not read a
//! module service: `BOSS_SOR_SERVICE=assets boss-api GET …` was refused
//! (no assets row in `infra/forge/sor-ports.env`, run 7d1ff56c), and
//! `GET /api/customers` answered 404 on the jobs port and 401 through
//! the gateway without a session. A car carrying a customer-data
//! migration sat HELD for want of one read-only production count. The
//! LAN door's table had grown a row at a time, each for a write or a
//! probe that needed it, and nothing asked about the services nobody had
//! needed yet — so the gap was found by the operator, at speed.
//!
//! THE PARTITION. boss-ports is the definition of a service. Three sets
//! must partition it — disjoint, and together the whole roster:
//!
//!   1. the LAN machine door, `infra/forge/sor-ports.env` (pinned to its
//!      manifest and route table by
//!      `the_machine_door_carries_every_read_surface.rs`);
//!   2. the pod's READ-ONLY rows, `infra/dev/sor-read-ports.env`, read on
//!      the in-cluster `boss-read-internal` Service, GET only;
//!   3. [`NO_DOOR`] — services that listen on loopback only, each with
//!      the reason it has no door.
//!
//! So a service added to boss-ports fails this test by name until it is
//! placed. And the read-only set is a fact that lives four times — the
//! table, the manifest, the read-route block of `sor-routes.sh`, the
//! host in `infra/dev/sor-read-url` — each held equal here (CLAUDE.md
//! §9a). The pin also holds the read Service to ClusterIP: a LoadBalancer
//! there would put header-trusted WRITE ports on the LAN, which is the
//! one thing this door was built not to do.

use boss_testing::repo_root;
use std::collections::{BTreeMap, BTreeSet};

const LAN_PORTS: &str = "infra/forge/sor-ports.env";
const READ_PORTS: &str = "infra/dev/sor-read-ports.env";
const READ_URL: &str = "infra/dev/sor-read-url";
const READ_MANIFEST: &str = "infra/cluster/manifests/boss-read-internal.yaml";
const ROUTES: &str = "infra/forge/probe-bin/sor-routes.sh";
const READ_ROUTES_BEGIN: &str = "# SOR-READ-ROUTES-BEGIN";
const READ_ROUTES_END: &str = "# SOR-READ-ROUTES-END";

/// The boss-ports services on NO door, and why. Each listens on
/// loopback only, so a Service port in front of it would be a port that
/// refuses — a door that answers "connection refused" about a surface
/// that exists. Widening any of these binds is its own change; when one
/// lands, move the row to a door.
const NO_DOOR: [(&str, &str); 4] = [
    (
        "search",
        "boss_search_api binds 127.0.0.1 (src/bin/boss_search_api.rs); the gateway fronts it",
    ),
    (
        "views",
        "boss_views_api binds 127.0.0.1 (src/bin/boss_views_api.rs); the gateway fronts it",
    ),
    (
        "simulator",
        "boss-simulator binds 127.0.0.1 by default; the gateway fronts /simulator",
    ),
    (
        "sim-control",
        "the brewery-sim daemon's localhost-only control port, proxied by boss-simulator",
    ),
];

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{} is readable: {e}", p.display()))
}

/// A `name=port` table, `#` comments and blanks ignored — the way the
/// doors read it.
fn table(rel: &str) -> BTreeMap<String, u16> {
    let mut out = BTreeMap::new();
    for line in read(rel).lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, port) = line
            .split_once('=')
            .unwrap_or_else(|| panic!("{rel}: `{line}` is not `name=port`"));
        let port: u16 = port
            .parse()
            .unwrap_or_else(|e| panic!("{rel}: `{line}` port is not a u16: {e}"));
        assert!(
            out.insert(name.to_string(), port).is_none(),
            "{rel}: `{name}` appears twice"
        );
    }
    assert!(!out.is_empty(), "{rel} names no port at all");
    out
}

fn roster() -> BTreeMap<String, u16> {
    boss_ports::all()
        .map(|s| (s.name.to_string(), s.prod))
        .collect()
}

/// The read Service's ports, `name -> (port, targetPort)`, from its
/// one-line flow entries.
fn manifest_ports() -> BTreeMap<String, (u16, u16)> {
    let mut out = BTreeMap::new();
    for line in read(READ_MANIFEST).lines().map(str::trim) {
        let Some(body) = line.strip_prefix("- {").and_then(|l| l.strip_suffix('}')) else {
            continue;
        };
        let fields: BTreeMap<&str, &str> = body
            .split(',')
            .filter_map(|kv| kv.split_once(':'))
            .map(|(k, v)| (k.trim(), v.trim()))
            .collect();
        let num = |k: &str| -> u16 {
            fields
                .get(k)
                .unwrap_or_else(|| panic!("{READ_MANIFEST}: `{line}` has no `{k}`"))
                .parse()
                .unwrap_or_else(|e| panic!("{READ_MANIFEST}: `{line}` {k}: {e}"))
        };
        let name = fields
            .get("name")
            .unwrap_or_else(|| panic!("{READ_MANIFEST}: `{line}` has no name"))
            .to_string();
        assert!(
            out.insert(name, (num("port"), num("targetPort"))).is_none(),
            "{READ_MANIFEST}: port name repeated in `{line}`"
        );
    }
    assert!(
        !out.is_empty(),
        "{READ_MANIFEST}: no `- {{port: …, targetPort: …, name: …}}` entries parsed"
    );
    // A port in any OTHER spelling is a port this parse cannot see
    // (review 1d893c98 N2: a block-style `- port: 7900` entry passed the
    // equality below unseen). Every `port:` key, in either spelling,
    // must be one of the flow entries parsed above.
    let port_keys = yaml_keys().iter().filter(|(k, _)| k == "port").count();
    assert_eq!(
        port_keys,
        out.len(),
        "{READ_MANIFEST} carries {port_keys} `port:` keys and {} one-line `- {{port: …}}` \
         entries — write every port entry on one flow-style line, the shape this pin reads",
        out.len()
    );
    out
}

/// Every `key: value` pair in the manifest outside a comment, in BOTH
/// YAML spellings — block (`key: v` on its own line, after an optional
/// `- `) and flow (`{a: 1, b: 2}`) — as `(key, value)`. Deliberately
/// cruder than a YAML parser and wider than it needs to be: a key this
/// sees that the parser would not is a false alarm someone reads, and a
/// key a parser sees that this would not is a way out of the pin.
fn yaml_keys() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in read(READ_MANIFEST).lines() {
        let line = line.split_once('#').map_or(line, |(code, _)| code);
        for part in line.split([',', '{', '}', '[', ']']) {
            let part = part.trim().trim_start_matches("- ").trim();
            if let Some((k, v)) = part.split_once(':') {
                let k = k.trim();
                if !k.is_empty() && !k.contains(' ') && !k.starts_with('-') {
                    out.push((k.to_string(), v.trim().to_string()));
                }
            }
        }
    }
    out
}

/// The first `key: value` at `indent` spaces, as written.
fn manifest_field(key: &str, indent: usize) -> Option<String> {
    let prefix = format!("{}{key}: ", " ".repeat(indent));
    read(READ_MANIFEST)
        .lines()
        .find_map(|l| l.strip_prefix(&prefix).map(|v| v.trim().to_string()))
}

/// The services between the read-route markers of the route file.
fn read_route_services() -> BTreeSet<String> {
    let sh = read(ROUTES);
    let block = sh
        .split_once(READ_ROUTES_BEGIN)
        .unwrap_or_else(|| panic!("{ROUTES} has no {READ_ROUTES_BEGIN} marker"))
        .1
        .split_once(READ_ROUTES_END)
        .unwrap_or_else(|| panic!("{ROUTES} has no {READ_ROUTES_END} marker"))
        .0;
    block
        .split_whitespace()
        .filter_map(|w| w.strip_prefix("service="))
        .map(|w| w.trim_end_matches(';').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// THE PARTITION: LAN rows, read-only rows and [`NO_DOOR`] are disjoint
/// and together are boss-ports — named, on each failure.
#[test]
fn the_doors_and_the_named_exceptions_partition_boss_ports() {
    let roster = roster();
    let lan = table(LAN_PORTS);
    let read_only = table(READ_PORTS);
    let none: BTreeSet<String> = NO_DOOR.iter().map(|(n, _)| n.to_string()).collect();

    for name in roster.keys() {
        let homes = [
            lan.contains_key(name),
            read_only.contains_key(name),
            none.contains(name),
        ]
        .iter()
        .filter(|b| **b)
        .count();
        assert_eq!(
            homes, 1,
            "boss-ports service `{name}` is on {homes} of: {LAN_PORTS}, {READ_PORTS}, NO_DOOR — \
             it must be on exactly one (a service with no door is the gap backlog 9a440539 \
             found by hand; a service on two is a fact living twice)"
        );
    }
    for name in lan.keys().chain(read_only.keys()).chain(none.iter()) {
        assert!(
            roster.contains_key(name),
            "`{name}` is on a door or in NO_DOOR, and boss-ports does not name it"
        );
    }
    for (name, port) in &read_only {
        assert_eq!(
            Some(port),
            roster.get(name),
            "{READ_PORTS}: `{name}` is not on its boss-ports prod port"
        );
    }
}

/// THE READ SERVICE: in-cluster only, its ports exactly the read table,
/// and the pod's read host is that Service's in-cluster name.
#[test]
fn the_read_service_is_the_read_table_and_stays_in_the_cluster() {
    let read_only = table(READ_PORTS);
    let manifest = manifest_ports();
    for (name, (port, target)) in &manifest {
        assert_eq!(
            port, target,
            "{READ_MANIFEST}: `{name}` maps {port} to {target} — the all-in-one pod listens on \
             the boss-ports port itself"
        );
    }
    let manifest: BTreeMap<String, u16> = manifest.into_iter().map(|(n, (p, _))| (n, p)).collect();
    assert_eq!(
        manifest, read_only,
        "{READ_MANIFEST} has drifted from {READ_PORTS} (left: the manifest; right: the table)"
    );

    // IN-CLUSTER ONLY, every way a Service can leave the cluster refused
    // (review 1d893c98 N1: an `externalIPs:` under `type: ClusterIP`
    // passed the first version of this check, and kube-proxy would
    // have accepted all ten header-trusted ports on that address). One
    // `type:` key, and it is ClusterIP; no external IP, node port,
    // load balancer field or external name anywhere outside a comment.
    let keys = yaml_keys();
    let types: Vec<&(String, String)> = keys.iter().filter(|(k, _)| k == "type").collect();
    assert_eq!(
        types.iter().map(|(_, v)| v.as_str()).collect::<Vec<_>>(),
        vec!["ClusterIP"],
        "{READ_MANIFEST} must carry exactly one `type:`, ClusterIP: a LoadBalancer or NodePort \
         would put header-trusted write ports on the LAN, which this door exists not to do"
    );
    for (key, _) in &keys {
        let k = key.to_ascii_lowercase();
        assert!(
            !(k == "externalips"
                || k == "nodeport"
                || k == "externalname"
                || k.starts_with("loadbalancer")
                || k.starts_with("externaltraffic")),
            "{READ_MANIFEST} sets `{key}:` — a way out of the cluster; the read door is \
             in-cluster only"
        );
    }

    let name = manifest_field("name", 2).expect("metadata.name");
    let namespace = manifest_field("namespace", 2).expect("metadata.namespace");
    assert_eq!(
        read(READ_URL).trim(),
        format!("http://{name}.{namespace}.svc.cluster.local"),
        "{READ_URL} must name {READ_MANIFEST}'s Service by its in-cluster name"
    );
}

/// THE PREFIXES: every read-only row is routed by a path (the read
/// block of the route file names exactly the read table's services),
/// and each prefix the gateway mounts for one of them lands on it.
#[test]
fn every_read_only_row_has_a_path_route_and_the_prefixes_land() {
    let read_only: BTreeSet<String> = table(READ_PORTS).into_keys().collect();
    assert_eq!(
        read_route_services(),
        read_only,
        "{ROUTES}'s {READ_ROUTES_BEGIN} block and {READ_PORTS} name different services"
    );

    let cases = [
        ("/api/customers", "customers"),
        ("/api/customers?email=x@example.test", "customers"),
        ("/api/customers/c-1", "customers"),
        ("/api/assets", "assets"),
        ("/api/assets?account_id=a-1", "assets"),
        ("/api/assets/tickets", "assets"),
        ("/api/catalog/models", "catalog"),
        ("/api/commerce/invoices", "commerce"),
        ("/api/inventory/items", "inventory"),
        ("/api/messages/threads", "messages"),
        ("/api/shipping/shipments", "shipping"),
        ("/api/products", "products"),
        ("/api/products/p-1", "products"),
        ("/api/campaigns", "campaigns"),
        ("/api/campaigns/c-1", "campaigns"),
        ("/api/clock/now", "clock"),
        // Look-alikes stay where they were.
        ("/api/customersx", "jobs"),
        ("/api/assetsx/1", "jobs"),
        ("/api/clock", "jobs"),
        // The LAN rows are untouched by the read block.
        ("/api/people/accounts", "accounts"),
        ("/api/people/emp-x", "people"),
        ("/api/jobs?kind=pr-train", "jobs"),
    ];
    let routes = repo_root().join(ROUTES);
    for (path, want) in cases {
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg("set -u; . \"$1\" && sor_service_for_path \"$2\"")
            .arg("sor-routes")
            .arg(&routes)
            .arg(path)
            .output()
            .expect("bash runs");
        assert!(
            out.status.success(),
            "sor_service_for_path {path}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            want,
            "sor_service_for_path {path}"
        );
    }
}
