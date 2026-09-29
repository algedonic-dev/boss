//! No role the converge applies grants `nodes/proxy` — by name, or by a
//! wildcard that covers it — and no binding hands a subject a
//! ClusterRole this render does not declare, whose rules nobody here can
//! read.
//!
//! WHY (backlog eeac3d56, 2026-09-29). ClusterRole `estate-observer`
//! granted `get` on `nodes/proxy` so its CronJob could read each
//! kubelet's /stats/summary, and its header reasoned that the kubelet's
//! write paths were all POST and so out of reach. They are not: the
//! kubelet authorizes a WebSocket exec, attach or port-forward opened as
//! a GET as `get nodes/proxy`, so the grant was exec into ANY pod on ANY
//! node — the boss pod, whose environment carries the credential
//! broker's root tokens, included. RBAC cannot narrow a proxy grant by
//! path, so there is no safe form of it; the free-space reading moved to
//! the forge (infra/estate/observe-nodefs.sh) and the grant went. This
//! pin keeps it from coming back through any role, for any verb: `create`
//! reaches the POST exec path, and every other verb is no narrower.
//!
//! WHAT COUNTS. A rule in the core group (`""`, `*`, or no apiGroups at
//! all — which is not a narrower rule) whose resources name
//! `nodes/proxy`, `nodes/*`, `*/proxy` or `*`. Kubernetes' own
//! cluster-admin and system:kubelet-api-admin hold it by default and are
//! not in this tree; a binding to ANY ClusterRole this render does not
//! declare is named as unjudgeable, because a built-in bound here would
//! carry the grant in without a line of it in the tree — unless the
//! role is on [`BUILT_IN_WITHOUT_NODE_PROXY`], with the reason.
//!
//! WHAT IS READ. The converge applies the RENDER, not the directory
//! (`render-instance.sh --all`, every instance namespace), so that is
//! what is judged — the same reason `no_default_service_account_reads_a_secret.rs`
//! gives. The YAML reader below is that file's, narrowed to roles and
//! bindings, and it REFUSES a shape it cannot read rather than passing
//! it. It is a COPY, and a copy is a holding action (CLAUDE.md §9a): the
//! shared RBAC reader on car cbb56130 (`boss_testing::rbac`, not on main
//! when this landed) is where this pin belongs once it lands — a
//! `credential_reach` rule for nodes/proxy — and then this file goes.

use boss_testing::{repo_root, scratch_dir};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const RENDER: &str = "infra/cluster/render-instance.sh";
const INSTANCES: &str = "infra/cluster/instances.toml";

/// Built-in ClusterRoles a binding in this tree may name although the
/// render does not declare them, each with why it cannot reach the
/// kubelet proxy. Empty today: add a row only with the upstream rule set
/// it was read from.
const BUILT_IN_WITHOUT_NODE_PROXY: &[(&str, &str)] = &[];

#[derive(Clone)]
struct Doc {
    file: String,
    lines: Vec<String>,
}

fn indent(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

fn scalar(v: &str) -> String {
    let v = v.split(" #").next().unwrap_or("").trim();
    v.trim_matches('"').trim_matches('\'').to_string()
}

fn flow_list(v: &str) -> Vec<String> {
    let v = v.split(" #").next().unwrap_or("").trim();
    match v.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        Some(inner) if inner.trim().is_empty() => Vec::new(),
        Some(inner) => inner.split(',').map(scalar).collect(),
        None if v.is_empty() => Vec::new(),
        None => vec![scalar(v)],
    }
}

fn docs_of(file: &str, text: &str) -> Vec<Doc> {
    text.split('\n')
        .fold(vec![Vec::<String>::new()], |mut acc, l| {
            if l.trim_end() == "---" {
                acc.push(Vec::new());
            } else if !l.trim().is_empty() && !l.trim_start().starts_with('#') {
                acc.last_mut().unwrap().push(l.to_string());
            }
            acc
        })
        .into_iter()
        .filter(|ls| !ls.is_empty())
        .map(|lines| Doc {
            file: file.to_string(),
            lines,
        })
        .collect()
}

impl Doc {
    fn top(&self, key: &str) -> Option<String> {
        let p = format!("{key}:");
        self.lines
            .iter()
            .find(|l| indent(l) == 0 && l.starts_with(&p))
            .map(|l| scalar(&l[p.len()..]))
    }

    fn section(&self, section: &str) -> Vec<String> {
        let head = format!("{section}:");
        self.lines
            .iter()
            .skip_while(|l| !(indent(l) == 0 && l.trim_end() == head))
            .skip(1)
            .take_while(|l| indent(l) > 0)
            .cloned()
            .collect()
    }

    fn child(&self, section: &str, key: &str) -> Option<String> {
        let lines = self.section(section);
        let depth = lines.iter().map(|l| indent(l)).min()?;
        let p = format!("{key}:");
        lines
            .iter()
            .find(|l| indent(l) == depth && l.trim_start().starts_with(&p))
            .map(|l| scalar(&l.trim_start()[p.len()..]))
    }

    fn kind(&self) -> String {
        self.top("kind").unwrap_or_default()
    }
    fn name(&self) -> String {
        self.child("metadata", "name").unwrap_or_default()
    }
    fn label(&self) -> String {
        format!(
            "{} {}/{} ({})",
            self.kind(),
            self.child("metadata", "namespace")
                .unwrap_or_else(|| "-".into()),
            self.name(),
            self.file
        )
    }

    /// The items of a top-level sequence, each a map of key -> values, in
    /// flow or block form. Panics, naming the object, on a shape it cannot
    /// read: an unreadable grant is refused, not passed.
    fn items(&self, section: &str) -> Vec<BTreeMap<String, Vec<String>>> {
        let lines = self.section(section);
        let Some(dash) = lines
            .iter()
            .find(|l| l.trim_start().starts_with("- "))
            .map(|l| indent(l))
        else {
            assert!(
                lines.is_empty(),
                "{}: `{section}:` holds lines this reader cannot read as a list — \
                 an unreadable grant is refused, not passed: {lines:?}",
                self.label()
            );
            return Vec::new();
        };
        let mut out: Vec<BTreeMap<String, Vec<String>>> = Vec::new();
        let mut open_key: Option<String> = None;
        for l in &lines {
            let t = l.trim_start();
            let (body, starts_item) = if indent(l) == dash && t.starts_with("- ") {
                (t[2..].trim_start(), true)
            } else if indent(l) > dash && t.starts_with("- ") && open_key.is_some() {
                let k = open_key.clone().unwrap();
                out.last_mut()
                    .expect("a block value belongs to an item")
                    .entry(k)
                    .or_default()
                    .push(scalar(&t[2..]));
                continue;
            } else {
                (t, false)
            };
            if starts_item {
                out.push(BTreeMap::new());
            }
            let (k, v) = body.split_once(':').unwrap_or_else(|| {
                panic!(
                    "{}: `{section}:` line {l:?} is not `key: value` — refused, not passed",
                    self.label()
                )
            });
            let vals = flow_list(v);
            open_key = vals.is_empty().then(|| k.trim().to_string());
            out.last_mut()
                .unwrap_or_else(|| panic!("{}: `{section}:` value before any item", self.label()))
                .insert(k.trim().to_string(), vals);
        }
        out
    }
}

fn any_of(vals: Option<&Vec<String>>, wanted: &[&str]) -> bool {
    vals.is_some_and(|vs| vs.iter().any(|v| wanted.contains(&v.as_str())))
}

/// The resource names that cover `nodes/proxy`.
const REACHES_NODE_PROXY: &[&str] = &["nodes/proxy", "nodes/*", "*/proxy", "*"];

/// Each rule of a Role/ClusterRole that reaches the kubelet proxy, as a
/// line naming the role and the resources.
fn node_proxy_grants(role: &Doc) -> Vec<String> {
    role.items("rules")
        .iter()
        .filter(|r| !r.contains_key("nonResourceURLs"))
        .filter(|r| !r.contains_key("apiGroups") || any_of(r.get("apiGroups"), &["", "*"]))
        .filter(|r| any_of(r.get("resources"), REACHES_NODE_PROXY))
        .map(|r| {
            format!(
                "{} grants resources {:?} (verbs {:?})",
                role.label(),
                r.get("resources").cloned().unwrap_or_default(),
                r.get("verbs").cloned().unwrap_or_default()
            )
        })
        .collect()
}

/// Every role granting it, and every binding naming a ClusterRole the
/// set does not declare (and the allowlist does not vouch for).
fn offenders(docs: &[Doc]) -> Vec<String> {
    let roles = docs
        .iter()
        .filter(|d| matches!(d.kind().as_str(), "Role" | "ClusterRole"))
        .flat_map(node_proxy_grants);
    let unjudged = docs
        .iter()
        .filter(|d| matches!(d.kind().as_str(), "RoleBinding" | "ClusterRoleBinding"))
        .filter(|b| b.child("roleRef", "kind").as_deref() == Some("ClusterRole"))
        .filter_map(|b| {
            let name = b.child("roleRef", "name").unwrap_or_default();
            let declared = docs
                .iter()
                .any(|d| d.kind() == "ClusterRole" && d.name() == name);
            let vouched = BUILT_IN_WITHOUT_NODE_PROXY.iter().any(|(n, _)| *n == name);
            (!declared && !vouched).then(|| {
                format!(
                    "{} binds ClusterRole {name}, which this render does not declare — \
                     a built-in (cluster-admin, system:kubelet-api-admin) carries nodes/proxy \
                     in without a line of it here",
                    b.label()
                )
            })
        });
    roles.chain(unjudged).collect()
}

fn render_all(case: &str) -> PathBuf {
    let out = scratch_dir(case).join("out");
    let run = Command::new("bash")
        .arg(repo_root().join(RENDER))
        .args(["--all", out.to_str().unwrap()])
        .env("BOSS_CLUSTER_TREE", repo_root())
        .output()
        .expect("bash runs the renderer");
    assert!(
        run.status.success(),
        "render-instance.sh --all: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    out
}

fn instance_namespaces() -> Vec<String> {
    let text = std::fs::read_to_string(repo_root().join(INSTANCES)).expect("instances.toml");
    let v: toml::Value = toml::from_str(&text).expect("instances.toml parses");
    v.as_table()
        .unwrap()
        .values()
        .filter_map(|t| t.get("namespace").and_then(|n| n.as_str()))
        .map(str::to_string)
        .collect()
}

fn docs_in(dir: &Path) -> Vec<Doc> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .collect();
    names.sort();
    names
        .iter()
        .flat_map(|p| {
            let label = format!(
                "{}/{}",
                p.parent().unwrap().file_name().unwrap().to_string_lossy(),
                p.file_name().unwrap().to_string_lossy()
            );
            docs_of(&label, &std::fs::read_to_string(p).unwrap())
        })
        .collect()
}

#[test]
fn no_role_in_the_render_reaches_the_kubelet_proxy() {
    let out = render_all("no-sa-reaches-kubelet-proxy");
    let all: Vec<Doc> = instance_namespaces()
        .iter()
        .flat_map(|ns| docs_in(&out.join(ns)))
        .collect();
    // A control: the reader must see the observer's ClusterRole and read
    // its one rule, or an empty verdict is the reader failing, not the
    // tree passing.
    let observer = all
        .iter()
        .find(|d| d.kind() == "ClusterRole" && d.name() == "estate-observer")
        .expect("the render carries ClusterRole estate-observer");
    let rules = observer.items("rules");
    assert!(
        rules.iter().any(|r| any_of(r.get("resources"), &["nodes"])),
        "the reader must read estate-observer's rule on nodes: {rules:?}"
    );
    let found = offenders(&all);
    assert!(
        found.is_empty(),
        "nodes/proxy is exec into any pod on any node (a WebSocket exec opened as a GET is \
         authorized as `get nodes/proxy`; backlog eeac3d56) — read node figures some other \
         way, as the estate observer does through the forge's talos-nodefs reading:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn the_reader_catches_every_shape_that_reaches_it() {
    let role = |rules: &str| {
        docs_of(
            "fixture.yaml",
            &format!(
                "apiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRole\nmetadata:\n  name: r\nrules:\n{rules}"
            ),
        )
    };
    for (why, rules) in [
        (
            "flow list, get",
            "  - apiGroups: [\"\"]\n    resources: [nodes, nodes/proxy]\n    verbs: [get, list]\n",
        ),
        (
            "block list, create",
            "  - apiGroups:\n      - \"\"\n    resources:\n      - nodes/proxy\n    verbs:\n      - create\n",
        ),
        (
            "every node subresource",
            "  - apiGroups: [\"\"]\n    resources: [\"nodes/*\"]\n    verbs: [get]\n",
        ),
        (
            "every resource",
            "  - apiGroups: [\"*\"]\n    resources: [\"*\"]\n    verbs: [\"*\"]\n",
        ),
        (
            "no apiGroups key",
            "  - resources: [nodes/proxy]\n    verbs: [get]\n",
        ),
    ] {
        assert_eq!(offenders(&role(rules)).len(), 1, "{why} must be caught");
    }
    // And what it must NOT catch: the observer's own rule, and a proxy
    // subresource of another group's resource.
    assert!(
        offenders(&role(
            "  - apiGroups: [\"\"]\n    resources: [nodes]\n    verbs: [get, list]\n"
        ))
        .is_empty()
    );
    assert!(
        offenders(&role(
            "  - apiGroups: [apps]\n    resources: [deployments]\n    verbs: [get]\n"
        ))
        .is_empty()
    );
}

#[test]
fn a_binding_to_a_cluster_role_the_render_does_not_declare_is_named() {
    let docs = docs_of(
        "fixture.yaml",
        "apiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRoleBinding\nmetadata:\n  name: b\n\
         roleRef:\n  apiGroup: rbac.authorization.k8s.io\n  kind: ClusterRole\n  name: cluster-admin\n\
         subjects:\n  - kind: ServiceAccount\n    name: x\n    namespace: boss\n",
    );
    let found = offenders(&docs);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("cluster-admin"), "{found:?}");
}
