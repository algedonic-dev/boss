//! Namespace `boss-node-maintenance` is bounded AT ADMISSION to one
//! workload: the trim CronJob, a Job and a Pod whose pod spec is the trim
//! pod, and nothing else (backlog 8eac4893; review 9b05cc55 N7 of the
//! scheduled-trim car 90d654c6).
//!
//! WHY. The trim car labels that namespace PodSecurity `privileged`, which
//! admits anything, and what stood in for a bound was a pin and lints that
//! read YAML line by line. Two reviews each found spellings a YAML decoder
//! reads and a line scan does not (16efea28, 9b05cc55). The API server
//! judges the decoded object whatever spelled it, so the bound is a
//! ValidatingAdmissionPolicy (infra/cluster/manifests/
//! boss-node-maintenance-admission.yaml) — and a SPELLING HAS NO EXISTENCE
//! THERE: a quoted or flow-style key, a `--- # x` separator, a lone CR, a
//! backslash-continued scalar, an explicit key and `!!binary` all arrive
//! as the object they decode to. So this pin holds DECISIONS on objects,
//! never the shape of a line.
//!
//! WHAT THIS HOLDS, by EVALUATING the manifest's own CEL:
//!   * the match: that one namespace by the label the API server itself
//!     sets, `scope: Namespaced` (without it a namespaceSelector does not
//!     exempt cluster-scoped objects and a deny-by-default policy would
//!     judge the whole cluster), every group, resource and subresource,
//!     CREATE, UPDATE and CONNECT, events alone excluded, failurePolicy
//!     Fail, a matchCondition on the request's namespace, and a binding
//!     that is exactly [Deny, Audit] with nothing narrowing it;
//!   * THE TRIM MANIFEST'S OWN POD IS THE ONE ADMITTED: its CronJob as
//!     declared and as the server holds it after defaulting, the Job a
//!     controller derives and the Pod that Job makes, the deny-all
//!     NetworkPolicy, and what the control plane writes into a namespace.
//!     The policy has to restate the digest, the script, the paths and
//!     every pod setting (it cannot read a manifest), so this is one fact
//!     in two files held equal (CLAUDE.md 9a): the expected values are
//!     READ from the trim manifest, never typed here;
//!   * about a hundred refusals, each by a validation ANSWERING FALSE under the
//!     message that names it: every single-field move of the trim pod, on
//!     all three carriers; every other kind and subresource; an UPDATE
//!     that moves a bounded field; smuggled documents in two of the
//!     spellings the reviews found, decoded and judged;
//!   * every validation in the policy is the one that refuses at least
//!     one of those, so none is dead text;
//!   * NO LIST OR MAP LITERAL MIXES VALUE TYPES, by a walk of the parsed
//!     expressions: the API server's CEL environment refuses to compile
//!     one and this evaluator does not notice (review 9dc7696c, F1 — the
//!     first cut carried four and was green here);
//!   * the matchCondition ANSWERS false for a request with no namespace,
//!     and a CONNECT with no object is refused by an answer;
//!   * the Namespace is declared here, for good, unprivileged until the
//!     trim manifest is in the tree — and that manifest declares none, so
//!     the converge always writes the policy before the CronJob.
//!
//! THE TRIM MANIFEST AND THE FIXTURE. This pin landed before the trim
//! manifest (car 90d654c6 was held), so the policy was written against a
//! fixture extracted mechanically from that car's head 0e13f736 (`git
//! show … | awk '/^---$/{n++} n>=1'`, the 148 lines three reviews held
//! byte-identical). The trim car's fold (2026-10-07) put
//! infra/cluster/manifests/boss-node-maintenance.yaml in the tree, and
//! from then every expected value here is read from THAT file, not the
//! fixture. The fixture is KEPT, and `the_fixture_is_the_trim_manifest`
//! holds the manifest's NetworkPolicy and CronJob equal to it: it is the
//! record of what the policy was reviewed against, so a manifest that
//! moves from the reviewed workload is red by name even where the policy
//! would still admit it (a schedule, a history limit — what the policy
//! does not bound). The fold dropped the manifest's Namespace document
//! and changed nothing else in it.
//!
//! WHAT IT CANNOT HOLD, stated: the evaluator is cel-rust, not the API
//! server's cel-go. cel-rust has NO TYPE CHECKER, so anything the
//! server's compiler refuses is invisible here unless a rule of this pin
//! looks for it by name; the literal rule is the one such rule, and the
//! manifest's header lists what else the server's environment restricts
//! and how each construct used stands. That list is a reading, not a
//! run. The "as the server holds it" objects are a MODEL
//! of defaulting (checked against a live v1.36.2 CronJob pod's own key
//! set on 2026-10-07, not against this pod). That the server compiles the
//! policy and decides as this pin does is shown only by a server-side dry
//! run, which the gate cannot make.
//!
//! tree-wide pin — it reads infra/cluster/manifests/, which no
//! changed-file map attributes to this crate, so every scoped gate runs
//! it whatever its scope (`tree_wide_pins` in infra/gate.sh).

use boss_testing::rbac::{Node, Object, Value as Yaml, read_stream};
use boss_testing::repo_root;
use cel::{Context, Program, Value as Cel};
use serde_json::{Value as Json, json};
use std::collections::{BTreeSet, HashMap};

const MANIFEST: &str = "infra/cluster/manifests/boss-node-maintenance-admission.yaml";
const TRIM: &str = "infra/cluster/manifests/boss-node-maintenance.yaml";
const TRIM_FIXTURE: &str =
    "crates/core/boss-testing/tests/fixtures/node-maintenance/boss-node-maintenance-0e13f736.yaml";
const POLICY: &str = "node-maintenance-holds-one-workload";
const NAMESPACE: &str = "boss-node-maintenance";

// ---- reading ----------------------------------------------------------------

fn read(path: &str) -> (String, Vec<Object>) {
    let text =
        std::fs::read_to_string(repo_root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    let objects = read_stream(path, &text).unwrap_or_else(|e| panic!("{e}"));
    (text, objects)
}

/// A block scalar's text EXACTLY as YAML yields it (`|`, clip chomping):
/// the shared reader skips it by design, and the trim script is compared
/// byte for byte, so indentation inside the block is kept.
fn block(file: &str, line: usize) -> String {
    let lines: Vec<&str> = file.lines().collect();
    let header = lines[line - 1];
    assert!(
        header.trim_end().ends_with('|'),
        "line {line} is not a plain `|` block scalar header: {header:?}"
    );
    let indent = |l: &str| l.len() - l.trim_start_matches(' ').len();
    // The parent's column: the `-` of a sequence entry that is the scalar
    // itself, else the key's.
    let lead = indent(header);
    let rest = &header[lead..];
    let parent = if rest.trim_end() == "- |" {
        lead
    } else {
        lead + (rest.len() - rest.trim_start_matches(['-', ' ']).len())
    };
    let body: Vec<&str> = lines[line..]
        .iter()
        .take_while(|l| l.trim().is_empty() || indent(l) > parent)
        .copied()
        .collect();
    let at = body
        .iter()
        .find(|l| !l.trim().is_empty())
        .map(|l| indent(l))
        .unwrap_or_else(|| panic!("line {line}: an empty block scalar"));
    let mut out: Vec<&str> = body
        .iter()
        .map(|l| if l.trim().is_empty() { "" } else { &l[at..] })
        .collect();
    while out.last() == Some(&"") {
        out.pop();
    }
    out.join("\n") + "\n"
}

/// A manifest node as the API server's decoder types it: a plain `true`
/// or `false` is a boolean and a run of digits an integer (no manifest
/// read here writes either as a quoted string), a block scalar is its
/// text, everything else a string.
fn typed(file: &str, node: &Node) -> Json {
    match &node.value {
        Yaml::Map(entries) => Json::Object(
            entries
                .iter()
                .map(|(k, n)| (k.clone(), typed(file, n)))
                .collect(),
        ),
        Yaml::Seq(items) => Json::Array(items.iter().map(|n| typed(file, n)).collect()),
        Yaml::Str(s) => match s.as_str() {
            "true" => json!(true),
            "false" => json!(false),
            d if !d.is_empty() && d.len() < 10 && d.bytes().all(|b| b.is_ascii_digit()) => {
                json!(d.parse::<i64>().expect("digits"))
            }
            _ => json!(s),
        },
        Yaml::Text => json!(block(file, node.line)),
        Yaml::Null => Json::Null,
    }
}

fn str_at<'a>(node: &'a Node, keys: &[&str]) -> &'a str {
    node.path(keys)
        .and_then(Node::str)
        .unwrap_or_else(|| panic!("no scalar at {keys:?}"))
}

fn items<'a>(node: &'a Node, keys: &[&str]) -> &'a [Node] {
    match node.path(keys).map(|n| &n.value) {
        Some(Yaml::Seq(items)) => items,
        other => panic!("{keys:?} is not a sequence: {other:?}"),
    }
}

fn strings(node: &Node, keys: &[&str]) -> Vec<String> {
    items(node, keys)
        .iter()
        .map(|n| match &n.value {
            Yaml::Str(s) => s.clone(),
            // `""` — the core API group — reads as null in this reader.
            Yaml::Null => String::new(),
            other => panic!("{keys:?}: {other:?}"),
        })
        .collect()
}

fn keys(node: &Node) -> Vec<&str> {
    match &node.value {
        Yaml::Map(entries) => entries.iter().map(|(k, _)| k.as_str()).collect(),
        other => panic!("not a mapping: {other:?}"),
    }
}

// ---- the trim manifest ------------------------------------------------------

fn trim_in_tree() -> bool {
    repo_root().join(TRIM).exists()
}

/// The trim manifest's documents, typed: from the tree when the trim car
/// has landed, else from the fixture cut from its head.
fn trim_docs(path: &str) -> Vec<Json> {
    let (text, objects) = read(path);
    objects.iter().map(|o| typed(&text, &o.root)).collect()
}

fn trim(kind: &str) -> Json {
    let path = if trim_in_tree() { TRIM } else { TRIM_FIXTURE };
    trim_docs(path)
        .into_iter()
        .find(|d| d["kind"] == kind)
        .unwrap_or_else(|| panic!("{path} declares no {kind}"))
}

// ---- the policy -------------------------------------------------------------

struct Policy {
    conditions: Vec<String>,
    /// (name, expression), in declaration order.
    variables: Vec<(String, String)>,
    /// (expression, message)
    validations: Vec<(String, String)>,
}

fn policy_object() -> (String, Vec<Object>) {
    read(MANIFEST)
}

fn policy() -> Policy {
    let (text, objects) = policy_object();
    let vap = objects
        .iter()
        .find(|o| o.kind == "ValidatingAdmissionPolicy" && o.name == POLICY)
        .unwrap_or_else(|| panic!("{MANIFEST} declares no ValidatingAdmissionPolicy/{POLICY}"));
    let spec = vap.root.get("spec").expect("the policy has a spec");
    let expression = |n: &Node| {
        let e = n.get("expression").expect("expression");
        match &e.value {
            Yaml::Str(s) => s.clone(),
            Yaml::Text => block(&text, e.line),
            other => panic!("expression at line {}: {other:?}", e.line),
        }
    };
    Policy {
        conditions: items(spec, &["matchConditions"])
            .iter()
            .map(expression)
            .collect(),
        variables: items(spec, &["variables"])
            .iter()
            .map(|v| (str_at(v, &["name"]).to_string(), expression(v)))
            .collect(),
        validations: items(spec, &["validations"])
            .iter()
            .map(|v| (expression(v), str_at(v, &["message"]).to_string()))
            .collect(),
    }
}

/// One admission request, as the API server presents it to the policy.
#[derive(Clone)]
struct Request {
    group: &'static str,
    resource: &'static str,
    sub: &'static str,
    operation: &'static str,
    namespace: &'static str,
    object: Json,
    old: Json,
}

fn create(group: &'static str, resource: &'static str, object: &Json) -> Request {
    Request {
        group,
        resource,
        sub: "",
        operation: "CREATE",
        namespace: NAMESPACE,
        object: object.clone(),
        old: Json::Null,
    }
}

fn update(group: &'static str, resource: &'static str, old: &Json, new: &Json) -> Request {
    Request {
        operation: "UPDATE",
        old: old.clone(),
        ..create(group, resource, new)
    }
}

fn sub(mut request: Request, sub: &'static str, operation: &'static str) -> Request {
    request.sub = sub;
    request.operation = operation;
    request
}

/// Marks a verdict reached through an ERROR rather than an answer. Under
/// failurePolicy Fail the server refuses on both, but an error is where
/// cel-rust and cel-go may part ways, so no verdict here may rest on one.
const ERRORED: &str = "[ERRORED]";

fn eval(expr: &str, ctx: &Context) -> Result<Cel, String> {
    let program = Program::compile(expr).map_err(|e| format!("does not compile: {e}"))?;
    program.execute(ctx).map_err(|e| format!("errors: {e}"))
}

/// `None` when the policy does not match the request (its matchCondition
/// answered false); otherwise the message of every validation it fails.
fn judge(p: &Policy, r: &Request) -> Option<Vec<String>> {
    // The server's request omits an empty subResource and an empty
    // namespace (omitempty), which is why the policy asks `has(...)` of
    // both before it reads either (review 9dc7696c, F2).
    let mut request = json!({
        "resource": {"group": r.group, "version": "v1", "resource": r.resource},
        "operation": r.operation,
        "userInfo": {"username": "admin"},
    });
    if !r.namespace.is_empty() {
        request["namespace"] = json!(r.namespace);
    }
    if !r.sub.is_empty() {
        request["subResource"] = json!(r.sub);
    }
    let mut ctx = Context::default();
    ctx.add_variable("request", &request).expect("request");
    ctx.add_variable("object", &r.object).expect("object");
    ctx.add_variable("oldObject", &r.old).expect("oldObject");
    for condition in &p.conditions {
        match eval(condition, &ctx) {
            Ok(Cel::Bool(true)) => {}
            Ok(Cel::Bool(false)) => return None,
            other => panic!("matchCondition {condition:?} answered {other:?}"),
        }
    }
    // Lazy on the server, eager here: every variable is written to be
    // total, so one that errors is itself a finding.
    let mut vars: HashMap<String, Cel> = HashMap::new();
    for (name, expr) in &p.variables {
        match eval(expr, &ctx) {
            Ok(value) => {
                vars.insert(name.clone(), value);
                ctx.add_variable_from_value("variables", vars.clone());
            }
            Err(e) => return Some(vec![format!("variable {name} {ERRORED} {e}")]),
        }
    }
    Some(
        p.validations
            .iter()
            .filter_map(|(expr, message)| match eval(expr, &ctx) {
                Ok(Cel::Bool(true)) => None,
                Ok(Cel::Bool(false)) => Some(message.clone()),
                other => Some(format!("{message} {ERRORED} {other:?}")),
            })
            .collect(),
    )
}

fn assert_admitted(p: &Policy, what: &str, r: &Request) {
    assert_eq!(
        judge(p, r),
        Some(Vec::new()),
        "{what}: the policy must judge it and admit it"
    );
}

/// Refused by validations answering false — never through an error — and
/// among them one whose message carries `naming`. Returns the messages.
fn assert_refused(p: &Policy, what: &str, r: &Request, naming: &str) -> Vec<String> {
    let flagged = judge(p, r).unwrap_or_else(|| panic!("{what}: the policy did not judge it"));
    assert!(
        !flagged.is_empty(),
        "{what}: ADMITTED — the policy refuses nothing about it"
    );
    assert!(
        !flagged.iter().any(|m| m.contains(ERRORED)),
        "{what}: refused only through an error, not an answer: {flagged:?}"
    );
    assert!(
        flagged.iter().any(|m| m.contains(naming)),
        "{what}: refused, but not under a message naming {naming:?}: {flagged:?}"
    );
    flagged
}

// ---- what the server holds --------------------------------------------------

/// A pod spec after the API server's defaulting, as a controller's or the
/// converge's write reaches validating admission: a `false` host
/// namespace is dropped (omitempty), and the defaults are filled in.
fn served_pod_spec(declared: &Json) -> Json {
    let mut spec = declared.clone();
    let map = spec.as_object_mut().expect("a pod spec");
    for host in ["hostNetwork", "hostPID", "hostIPC"] {
        if map.get(host) == Some(&json!(false)) {
            map.remove(host);
        }
    }
    map.insert("dnsPolicy".into(), json!("ClusterFirst"));
    map.insert("schedulerName".into(), json!("default-scheduler"));
    map.insert("terminationGracePeriodSeconds".into(), json!(30));
    for c in spec["containers"].as_array_mut().expect("containers") {
        c["imagePullPolicy"] = json!("IfNotPresent");
        c["terminationMessagePath"] = json!("/dev/termination-log");
        c["terminationMessagePolicy"] = json!("File");
    }
    spec
}

fn cronjob() -> Json {
    trim("CronJob")
}

fn cronjob_served() -> Json {
    let mut cj = cronjob();
    let spec = &mut cj["spec"]["jobTemplate"]["spec"]["template"]["spec"];
    *spec = served_pod_spec(spec);
    cj["spec"]["suspend"] = json!(false);
    cj
}

/// The Job the CronJob controller creates from the template.
fn job() -> Json {
    let cj = cronjob_served();
    let mut spec = cj["spec"]["jobTemplate"]["spec"].clone();
    let uid = "0f0e0d0c-0b0a-4908-8706-050403020100";
    let name = "boss-node-trim-29852925";
    spec["completions"] = json!(1);
    spec["parallelism"] = json!(1);
    spec["completionMode"] = json!("NonIndexed");
    spec["suspend"] = json!(false);
    spec["selector"] = json!({"matchLabels": {"batch.kubernetes.io/controller-uid": uid}});
    let labels = spec["template"]["metadata"]["labels"]
        .as_object_mut()
        .expect("template labels");
    labels.insert("batch.kubernetes.io/controller-uid".into(), json!(uid));
    labels.insert("batch.kubernetes.io/job-name".into(), json!(name));
    json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": name,
            "namespace": NAMESPACE,
            "ownerReferences": [{"apiVersion": "batch/v1", "kind": "CronJob", "name": "boss-node-trim", "controller": true}],
        },
        "spec": spec,
    })
}

/// The Pod the Job controller creates, after the admission plugins that
/// fill in a ServiceAccount, tolerations and a priority.
fn pod() -> Json {
    let j = job();
    let mut spec = j["spec"]["template"]["spec"].clone();
    spec["serviceAccountName"] = json!("default");
    spec["serviceAccount"] = json!("default");
    spec["priority"] = json!(0);
    spec["preemptionPolicy"] = json!("PreemptLowerPriority");
    spec["tolerations"] = json!([
        {"effect": "NoExecute", "key": "node.kubernetes.io/not-ready", "operator": "Exists", "tolerationSeconds": 300},
        {"effect": "NoExecute", "key": "node.kubernetes.io/unreachable", "operator": "Exists", "tolerationSeconds": 300},
    ]);
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "generateName": "boss-node-trim-29852925-",
            "name": "boss-node-trim-29852925-x7k2p",
            "namespace": NAMESPACE,
            "labels": j["spec"]["template"]["metadata"]["labels"],
            "finalizers": ["batch.kubernetes.io/job-tracking"],
            "ownerReferences": [{"apiVersion": "batch/v1", "kind": "Job", "name": j["metadata"]["name"], "controller": true}],
        },
        "spec": spec,
    })
}

/// The three carriers of the one pod template: (label, group, resource,
/// the object, the JSON pointer of its template).
fn carriers() -> Vec<(&'static str, &'static str, &'static str, Json, &'static str)> {
    vec![
        (
            "CronJob",
            "batch",
            "cronjobs",
            cronjob_served(),
            "/spec/jobTemplate/spec/template",
        ),
        ("Job", "batch", "jobs", job(), "/spec/template"),
        ("Pod", "", "pods", pod(), ""),
    ]
}

fn template<'a>(object: &'a mut Json, pointer: &str) -> &'a mut Json {
    object
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("no template at {pointer:?}"))
}

type Edit = fn(&mut Json);

fn c0(t: &mut Json) -> &mut Json {
    &mut t["spec"]["containers"][0]
}

/// One move: a non-capturing closure coerces to `Edit` at this call.
fn m(what: &'static str, edit: Edit, naming: &'static str) -> (&'static str, Edit, &'static str) {
    (what, edit, naming)
}

/// Every single move away from the trim pod, with a phrase of the message
/// that must refuse it. `t` is the template: {metadata, spec}.
fn moves() -> Vec<(&'static str, Edit, &'static str)> {
    const SC: &str = "container securityContext is exactly";
    const PODSC: &str = "pod securityContext is exactly";
    const VOLUMES: &str = "mounts exactly two hostPath directories";
    const MOUNTS: &str = "mounts its two volumes read-only";
    const FIELD: &str = "a field this policy does not name (an init";
    const CFIELD: &str = "exactly one container, named trim";
    const IMAGE: &str = "pinned by digest";
    const RUN: &str = "runs /bin/sh -c and the trim script";
    const HOST: &str = "joins no host network, PID or IPC";
    const ONCE: &str = "mounts no token";
    const NODE: &str = "pinned to the build node";
    vec![
        m(
            "privileged: true",
            |t| c0(t)["securityContext"]["privileged"] = json!(true),
            SC,
        ),
        m(
            "privileged removed",
            |t| {
                c0(t)["securityContext"]
                    .as_object_mut()
                    .unwrap()
                    .remove("privileged");
            },
            SC,
        ),
        m(
            "a second capability added",
            |t| {
                c0(t)["securityContext"]["capabilities"]["add"] = json!(["SYS_ADMIN", "SYS_PTRACE"])
            },
            SC,
        ),
        m(
            "another capability instead",
            |t| c0(t)["securityContext"]["capabilities"]["add"] = json!(["NET_ADMIN"]),
            SC,
        ),
        m(
            "drop ALL removed",
            |t| {
                c0(t)["securityContext"]["capabilities"]
                    .as_object_mut()
                    .unwrap()
                    .remove("drop");
            },
            SC,
        ),
        m(
            "allowPrivilegeEscalation: true",
            |t| c0(t)["securityContext"]["allowPrivilegeEscalation"] = json!(true),
            SC,
        ),
        m(
            "a writable root filesystem",
            |t| c0(t)["securityContext"]["readOnlyRootFilesystem"] = json!(false),
            SC,
        ),
        m(
            "procMount: Unmasked",
            |t| c0(t)["securityContext"]["procMount"] = json!("Unmasked"),
            SC,
        ),
        m(
            "a container seccomp profile Unconfined",
            |t| c0(t)["securityContext"]["seccompProfile"] = json!({"type": "Unconfined"}),
            SC,
        ),
        m(
            "a container appArmorProfile Unconfined",
            |t| c0(t)["securityContext"]["appArmorProfile"] = json!({"type": "Unconfined"}),
            SC,
        ),
        m(
            "pod seccomp Unconfined",
            |t| t["spec"]["securityContext"]["seccompProfile"] = json!({"type": "Unconfined"}),
            PODSC,
        ),
        m(
            "pod seccomp Localhost",
            |t| {
                t["spec"]["securityContext"]["seccompProfile"] =
                    json!({"type": "Localhost", "localhostProfile": "x.json"})
            },
            PODSC,
        ),
        m(
            "a sysctl",
            |t| {
                t["spec"]["securityContext"]["sysctls"] =
                    json!([{"name": "kernel.core_pattern", "value": "|/x"}])
            },
            PODSC,
        ),
        m(
            "a supplemental group",
            |t| t["spec"]["securityContext"]["supplementalGroups"] = json!([6]),
            PODSC,
        ),
        m(
            "another gid",
            |t| t["spec"]["securityContext"]["runAsGroup"] = json!(6),
            PODSC,
        ),
        m(
            "no gid",
            |t| {
                t["spec"]["securityContext"]
                    .as_object_mut()
                    .unwrap()
                    .remove("runAsGroup");
            },
            PODSC,
        ),
        m(
            "runAsNonRoot: true",
            |t| t["spec"]["securityContext"]["runAsNonRoot"] = json!(true),
            PODSC,
        ),
        m(
            "no runAsNonRoot",
            |t| {
                t["spec"]["securityContext"]
                    .as_object_mut()
                    .unwrap()
                    .remove("runAsNonRoot");
            },
            PODSC,
        ),
        m(
            "no uid",
            |t| {
                t["spec"]["securityContext"]
                    .as_object_mut()
                    .unwrap()
                    .remove("runAsUser");
            },
            PODSC,
        ),
        m(
            "a volume with a second source beside its hostPath",
            |t| t["spec"]["volumes"][0]["secret"] = json!({"secretName": "x"}),
            VOLUMES,
        ),
        m(
            "the first volume renamed",
            |t| t["spec"]["volumes"][0]["name"] = json!("other"),
            VOLUMES,
        ),
        m(
            "the second volume renamed",
            |t| t["spec"]["volumes"][1]["name"] = json!("other"),
            VOLUMES,
        ),
        m(
            "a third mount, read-only",
            |t| {
                c0(t)["volumeMounts"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"name": "gate", "mountPath": "/x", "readOnly": true}))
            },
            MOUNTS,
        ),
        m(
            "a mount with a subPath",
            |t| c0(t)["volumeMounts"][0]["subPath"] = json!(".."),
            MOUNTS,
        ),
        m(
            "the first mount moved",
            |t| c0(t)["volumeMounts"][0]["mountPath"] = json!("/"),
            MOUNTS,
        ),
        m(
            "the second mount moved",
            |t| c0(t)["volumeMounts"][1]["mountPath"] = json!("/etc"),
            MOUNTS,
        ),
        m(
            "the first mount naming the other volume",
            |t| c0(t)["volumeMounts"][0]["name"] = json!("ephemeral"),
            MOUNTS,
        ),
        m(
            "the second mount naming the other volume",
            |t| c0(t)["volumeMounts"][1]["name"] = json!("gate"),
            MOUNTS,
        ),
        m(
            "another uid",
            |t| t["spec"]["securityContext"]["runAsUser"] = json!(1000),
            PODSC,
        ),
        m(
            "no pod securityContext",
            |t| {
                t["spec"].as_object_mut().unwrap().remove("securityContext");
            },
            PODSC,
        ),
        m(
            "hostNetwork: true",
            |t| t["spec"]["hostNetwork"] = json!(true),
            HOST,
        ),
        m(
            "hostPID: true",
            |t| t["spec"]["hostPID"] = json!(true),
            HOST,
        ),
        m(
            "hostIPC: true",
            |t| t["spec"]["hostIPC"] = json!(true),
            HOST,
        ),
        m(
            "a third volume, the node's root",
            |t| {
                t["spec"]["volumes"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"name": "root", "hostPath": {"path": "/"}}))
            },
            VOLUMES,
        ),
        m(
            "the gate path widened to its parent",
            |t| t["spec"]["volumes"][0]["hostPath"]["path"] = json!("/var/mnt/gate"),
            VOLUMES,
        ),
        m(
            "the ephemeral path widened to /var",
            |t| t["spec"]["volumes"][1]["hostPath"]["path"] = json!("/var"),
            VOLUMES,
        ),
        m(
            "a path with a trailing component",
            |t| t["spec"]["volumes"][0]["hostPath"]["path"] = json!("/var/mnt/gate/seed/../.."),
            VOLUMES,
        ),
        m(
            "type DirectoryOrCreate",
            |t| t["spec"]["volumes"][0]["hostPath"]["type"] = json!("DirectoryOrCreate"),
            VOLUMES,
        ),
        m(
            "a Secret volume",
            |t| {
                t["spec"]["volumes"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"name": "s", "secret": {"secretName": "x"}}))
            },
            VOLUMES,
        ),
        m(
            "a projected token volume",
            |t| {
                t["spec"]["volumes"].as_array_mut().unwrap().push(json!({"name": "t", "projected": {"sources": [{"serviceAccountToken": {"path": "token"}}]}}))
            },
            VOLUMES,
        ),
        m(
            "one volume swapped for an emptyDir",
            |t| t["spec"]["volumes"][1] = json!({"name": "ephemeral", "emptyDir": {}}),
            VOLUMES,
        ),
        m(
            "a mount made writable",
            |t| c0(t)["volumeMounts"][0]["readOnly"] = json!(false),
            MOUNTS,
        ),
        m(
            "a mount with readOnly removed",
            |t| {
                c0(t)["volumeMounts"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("readOnly");
            },
            MOUNTS,
        ),
        m(
            "Bidirectional mount propagation",
            |t| c0(t)["volumeMounts"][0]["mountPropagation"] = json!("Bidirectional"),
            MOUNTS,
        ),
        m(
            "a third mount",
            |t| {
                c0(t)["volumeMounts"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"name": "gate", "mountPath": "/x"}))
            },
            MOUNTS,
        ),
        m(
            "a device claim",
            |t| c0(t)["resources"]["claims"] = json!([{"name": "gpu"}]),
            MOUNTS,
        ),
        m(
            "a device-plugin resource in limits",
            |t| c0(t)["resources"]["limits"]["nvidia.com/gpu"] = json!(1),
            MOUNTS,
        ),
        m(
            "a device-plugin resource in requests",
            |t| c0(t)["resources"]["requests"]["nvidia.com/gpu"] = json!(1),
            MOUNTS,
        ),
        m(
            "a resource the trim manifest does not declare",
            |t| c0(t)["resources"]["limits"]["ephemeral-storage"] = json!("1Gi"),
            MOUNTS,
        ),
        m(
            "another termination message path",
            |t| c0(t)["terminationMessagePath"] = json!("/trim/gate/x"),
            MOUNTS,
        ),
        m(
            "another digest",
            |t| {
                c0(t)["image"] = json!(
                    "10.20.0.15:3000/david/alpine-k8s:1.33.3@sha256:0000000000000000000000000000000000000000000000000000000000000000"
                )
            },
            IMAGE,
        ),
        m(
            "a tag and no digest",
            |t| c0(t)["image"] = json!("10.20.0.15:3000/david/alpine-k8s:1.33.3"),
            IMAGE,
        ),
        m(
            "the digest as a tag",
            |t| {
                c0(t)["image"] = json!(
                    "evil/x:sha256-cdeda0da2cd6896023cc5f96505bc94a699a989dff2bfc3e2008ab92eab1e93a"
                )
            },
            IMAGE,
        ),
        m(
            "the digest then a second digest",
            |t| {
                let image = c0(t)["image"].as_str().unwrap().to_string();
                c0(t)["image"] = json!(format!(
                    "{image}@sha256:0000000000000000000000000000000000000000000000000000000000000000"
                ));
            },
            IMAGE,
        ),
        m(
            "the digest as a path, another digest pinned",
            |t| {
                c0(t)["image"] = json!(
                    "evil/@sha256:cdeda0da2cd6896023cc5f96505bc94a699a989dff2bfc3e2008ab92eab1e93a/x@sha256:0000000000000000000000000000000000000000000000000000000000000000"
                )
            },
            IMAGE,
        ),
        m(
            "the digest and no repository",
            |t| {
                let image = c0(t)["image"].as_str().unwrap().to_string();
                let digest = image.split_once('@').unwrap().1.to_string();
                c0(t)["image"] = json!(format!("@{digest}"));
            },
            IMAGE,
        ),
        m(
            "another digest, then the pinned one",
            |t| {
                let image = c0(t)["image"].as_str().unwrap().to_string();
                let digest = image.split_once('@').unwrap().1.to_string();
                c0(t)["image"] = json!(format!(
                    "evil/x@sha256:0000000000000000000000000000000000000000000000000000000000000000@{digest}"
                ));
            },
            IMAGE,
        ),
        m(
            "the digest with a trailing line",
            |t| {
                let image = c0(t)["image"].as_str().unwrap().to_string();
                c0(t)["image"] = json!(format!("{image}\nx"));
            },
            IMAGE,
        ),
        m(
            "another command",
            |t| c0(t)["command"] = json!(["/bin/sh", "-xc"]),
            RUN,
        ),
        m(
            "one more line of script",
            |t| {
                let script = c0(t)["args"][0].as_str().unwrap().to_string();
                c0(t)["args"] = json!([format!("mount -o remount,rw /trim/gate\n{script}")]);
            },
            RUN,
        ),
        m(
            "the script with one byte fewer",
            |t| {
                let script = c0(t)["args"][0].as_str().unwrap().to_string();
                c0(t)["args"] = json!([script.trim_end()]);
            },
            RUN,
        ),
        m(
            "a second argument",
            |t| c0(t)["args"].as_array_mut().unwrap().push(json!("x")),
            RUN,
        ),
        m(
            "no command (the image's entrypoint)",
            |t| {
                c0(t).as_object_mut().unwrap().remove("command");
            },
            RUN,
        ),
        m(
            "an env var",
            |t| c0(t)["env"] = json!([{"name": "TRIM_GATE", "value": "/"}]),
            CFIELD,
        ),
        m(
            "an envFrom",
            |t| c0(t)["envFrom"] = json!([{"secretRef": {"name": "x"}}]),
            CFIELD,
        ),
        m(
            "a postStart hook",
            |t| c0(t)["lifecycle"] = json!({"postStart": {"exec": {"command": ["sh", "-c", "x"]}}}),
            CFIELD,
        ),
        m(
            "an exec probe",
            |t| c0(t)["livenessProbe"] = json!({"exec": {"command": ["sh", "-c", "x"]}}),
            CFIELD,
        ),
        m("a tty", |t| c0(t)["tty"] = json!(true), CFIELD),
        m(
            "a block device",
            |t| c0(t)["volumeDevices"] = json!([{"name": "gate", "devicePath": "/dev/x"}]),
            CFIELD,
        ),
        m(
            "another container name",
            |t| c0(t)["name"] = json!("helper"),
            CFIELD,
        ),
        m(
            "a second container",
            |t| {
                t["spec"]["containers"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"name": "helper", "image": "busybox"}))
            },
            CFIELD,
        ),
        m(
            "no container",
            |t| t["spec"]["containers"] = json!([]),
            CFIELD,
        ),
        m(
            "an init container",
            |t| t["spec"]["initContainers"] = json!([{"name": "i", "image": "busybox"}]),
            FIELD,
        ),
        m(
            "an ephemeral container in the spec",
            |t| t["spec"]["ephemeralContainers"] = json!([{"name": "d", "image": "busybox"}]),
            FIELD,
        ),
        m(
            "a runtime class",
            |t| t["spec"]["runtimeClassName"] = json!("x"),
            FIELD,
        ),
        m(
            "a shared process namespace",
            |t| t["spec"]["shareProcessNamespace"] = json!(true),
            FIELD,
        ),
        m("hostUsers", |t| t["spec"]["hostUsers"] = json!(true), FIELD),
        m(
            "a pull secret",
            |t| t["spec"]["imagePullSecrets"] = json!([{"name": "x"}]),
            FIELD,
        ),
        m(
            "a resource claim",
            |t| t["spec"]["resourceClaims"] = json!([{"name": "gpu"}]),
            FIELD,
        ),
        m(
            "the token mounted",
            |t| t["spec"]["automountServiceAccountToken"] = json!(true),
            ONCE,
        ),
        m(
            "automount left to its default",
            |t| {
                t["spec"]
                    .as_object_mut()
                    .unwrap()
                    .remove("automountServiceAccountToken");
            },
            ONCE,
        ),
        m(
            "another ServiceAccount",
            |t| t["spec"]["serviceAccountName"] = json!("builder"),
            ONCE,
        ),
        m(
            "restartPolicy OnFailure",
            |t| t["spec"]["restartPolicy"] = json!("OnFailure"),
            ONCE,
        ),
        m(
            "another node selector",
            |t| t["spec"]["nodeSelector"] = json!({"kubernetes.io/hostname": "cp-1"}),
            NODE,
        ),
        m(
            "a second selector key",
            |t| t["spec"]["nodeSelector"]["x"] = json!("y"),
            NODE,
        ),
        m(
            "no node selector",
            |t| {
                t["spec"].as_object_mut().unwrap().remove("nodeSelector");
            },
            NODE,
        ),
        m(
            "a node named at create",
            |t| t["spec"]["nodeName"] = json!("cp-1"),
            NODE,
        ),
        m(
            "an AppArmor annotation",
            |t| {
                t["metadata"]["annotations"] =
                    json!({"container.apparmor.security.beta.kubernetes.io/trim": "unconfined"})
            },
            "AppArmor annotation",
        ),
    ]
}

// ---- the match --------------------------------------------------------------

#[test]
fn the_policy_matches_that_namespace_and_nothing_cluster_scoped() {
    let (_, objects) = policy_object();
    let vap = objects
        .iter()
        .find(|o| o.kind == "ValidatingAdmissionPolicy")
        .expect("a policy");
    assert_eq!(vap.name, POLICY);
    let spec = vap.root.get("spec").expect("spec");
    assert_eq!(
        keys(spec),
        [
            "failurePolicy",
            "matchConstraints",
            "matchConditions",
            "variables",
            "validations"
        ],
        "no paramKind (a parameter object is a second, writable copy of the bound) and no \
         auditAnnotations"
    );
    assert_eq!(
        str_at(spec, &["failurePolicy"]),
        "Fail",
        "an expression that errors must refuse, not admit"
    );
    let constraints = spec.get("matchConstraints").expect("matchConstraints");
    assert_eq!(
        keys(constraints),
        ["namespaceSelector", "resourceRules", "excludeResourceRules"],
        "no objectSelector (a label the caller writes would let an object opt out) and no \
         matchPolicy"
    );
    let selector = constraints.get("namespaceSelector").expect("selector");
    assert_eq!(keys(selector), ["matchLabels"]);
    assert_eq!(
        typed("", selector.get("matchLabels").expect("matchLabels")),
        json!({"kubernetes.io/metadata.name": NAMESPACE}),
        "the label the API server sets on every namespace and refuses to change"
    );
    let rules = items(constraints, &["resourceRules"]);
    assert_eq!(rules.len(), 1);
    let rule = &rules[0];
    assert_eq!(
        keys(rule),
        [
            "apiGroups",
            "apiVersions",
            "operations",
            "resources",
            "scope"
        ]
    );
    assert_eq!(strings(rule, &["apiGroups"]), ["*"]);
    assert_eq!(strings(rule, &["apiVersions"]), ["*"]);
    assert_eq!(
        strings(rule, &["resources"]),
        ["*/*"],
        "every resource AND subresource: `*` alone leaves pods/exec and \
         pods/ephemeralcontainers unjudged"
    );
    assert_eq!(
        strings(rule, &["operations"]),
        ["CREATE", "UPDATE", "CONNECT"],
        "CONNECT is exec, attach and port-forward; DELETE is never judged, so the way back \
         stays open"
    );
    assert_eq!(
        str_at(rule, &["scope"]),
        "Namespaced",
        "WITHOUT this a namespaceSelector does not exempt cluster-scoped objects: a \
         deny-by-default policy would refuse every node, ClusterRole and CRD write in the cluster"
    );
    let excluded = items(constraints, &["excludeResourceRules"]);
    assert_eq!(excluded.len(), 1, "events, and only events, are unjudged");
    assert_eq!(strings(&excluded[0], &["apiGroups"]), ["", "events.k8s.io"]);
    assert_eq!(strings(&excluded[0], &["resources"]), ["events"]);
    assert_eq!(str_at(&excluded[0], &["scope"]), "Namespaced");

    // The matchCondition is the third confinement, and it is EVALUATED:
    // a request in any other namespace is not judged at all.
    let p = policy();
    assert_eq!(p.conditions.len(), 1);
    let deployment = json!({"metadata": {"name": "boss"}, "spec": {}});
    for elsewhere in [
        "boss",
        "boss-dev",
        "kube-system",
        "boss-node-maintenance-2",
        "",
    ] {
        let mut r = create("apps", "deployments", &deployment);
        r.namespace = elsewhere;
        assert_eq!(
            judge(&p, &r),
            None,
            "a write in namespace {elsewhere:?} must not be judged"
        );
    }
    assert!(judge(&p, &create("apps", "deployments", &deployment)).is_some());
}

#[test]
fn the_binding_denies_and_narrows_nothing() {
    let (_, objects) = policy_object();
    let bindings: Vec<&Object> = objects
        .iter()
        .filter(|o| o.kind == "ValidatingAdmissionPolicyBinding")
        .collect();
    assert_eq!(bindings.len(), 1);
    let spec = bindings[0].root.get("spec").expect("spec");
    assert_eq!(
        keys(spec),
        ["policyName", "validationActions"],
        "no matchResources and no paramRef: either would let the binding judge less than the \
         policy matches"
    );
    assert_eq!(str_at(spec, &["policyName"]), POLICY);
    assert_eq!(
        strings(spec, &["validationActions"]),
        ["Deny", "Audit"],
        "Warn or Audit alone admits what the policy refuses"
    );
    let kinds: Vec<&str> = objects.iter().map(|o| o.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "ValidatingAdmissionPolicy",
            "ValidatingAdmissionPolicyBinding",
            "Namespace",
        ],
        "{MANIFEST} declares exactly these, the Namespace among them for good"
    );
}

/// The Namespace is declared HERE, for good, and nowhere else (review
/// 9dc7696c, F3). The converge applies every file carrying a Namespace
/// document first and is `set -e`: were the Namespace in the trim
/// manifest, its CronJob would be applied BEFORE this file, a car moving
/// the digest would meet the old policy, and the converge would end
/// before the new policy was written — on every tick. With the Namespace
/// here the policy is always written before the CronJob. Until the trim
/// manifest is in the tree the namespace is `baseline`; the trim car
/// changes that one line.
#[test]
fn the_namespace_is_declared_here_and_unprivileged_until_the_trim_lands() {
    let (_, objects) = policy_object();
    let here: Vec<&Object> = objects.iter().filter(|o| o.kind == "Namespace").collect();
    assert_eq!(here.len(), 1, "{MANIFEST} declares the Namespace");
    assert_eq!(here[0].name, NAMESPACE);
    let level = typed(
        "",
        here[0].root.path(&["metadata", "labels"]).expect("labels"),
    )["pod-security.kubernetes.io/enforce"]
        .clone();
    if trim_in_tree() {
        let (text, there) = read(TRIM);
        let declared: Vec<&Object> = there.iter().filter(|o| o.kind == "Namespace").collect();
        assert!(
            declared.is_empty() && !text.lines().any(|l| l == "kind: Namespace"),
            "{TRIM} declares a Namespace: the converge would then apply its CronJob in the \
             namespace phase, BEFORE the policy in {MANIFEST} — a digest or script bump could \
             never converge. The Namespace is declared in {MANIFEST} only; the trim car changes \
             its `enforce` line there"
        );
        assert!(
            level == "privileged" || level == "baseline",
            "enforce is {level}"
        );
        return;
    }
    assert_eq!(
        level, "baseline",
        "until the trim car lands this namespace admits no hostPath and no capability: the \
         policy is exercised live before any privileged admission exists"
    );
    for (key, node) in here[0].root.walk() {
        assert!(
            !(key.starts_with("pod-security.kubernetes.io/") && node.str() == Some("privileged")),
            "the Namespace here carries `{key}: privileged`"
        );
    }
}

// ---- admitted ---------------------------------------------------------------

#[test]
fn the_trim_manifests_own_workload_is_admitted() {
    let p = policy();
    assert_admitted(
        &p,
        "the CronJob as the manifest declares it",
        &create("batch", "cronjobs", &cronjob()),
    );
    for (label, group, resource, object, _) in carriers() {
        assert_admitted(
            &p,
            &format!("the {label} as the server holds it"),
            &create(group, resource, &object),
        );
    }
    // The script the policy carries IS the manifest's, byte for byte.
    let script =
        &cronjob()["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"][0]["args"][0];
    assert!(
        script
            .as_str()
            .is_some_and(|s| s.starts_with("set -u\n") && s.ends_with("exit \"$rc\"\n")),
        "the trim script was not read whole: {script:?}"
    );
    assert_admitted(
        &p,
        "the deny-all NetworkPolicy",
        &create(
            "networking.k8s.io",
            "networkpolicies",
            &trim("NetworkPolicy"),
        ),
    );
}

#[test]
fn what_the_control_plane_writes_for_the_one_pod_is_admitted() {
    let p = policy();
    let named = |name: &str| json!({"metadata": {"name": name, "namespace": NAMESPACE}});
    assert_admitted(
        &p,
        "the default ServiceAccount",
        &create("", "serviceaccounts", &named("default")),
    );
    assert_admitted(
        &p,
        "the root-CA ConfigMap",
        &create("", "configmaps", &named("kube-root-ca.crt")),
    );
    for (label, group, resource, object, _) in carriers() {
        // A status write of a workload that would NOT pass as a spec write
        // (an old digest) still passes: status holds no privilege.
        let mut old = object.clone();
        old["status"] = json!({"x": 1});
        assert_admitted(
            &p,
            &format!("{label} status"),
            &sub(update(group, resource, &object, &old), "status", "UPDATE"),
        );
    }
    let p0 = pod();
    assert_admitted(
        &p,
        "the scheduler's binding",
        &sub(
            create(
                "",
                "pods",
                &json!({"metadata": {"name": p0["metadata"]["name"]}, "target": {"kind": "Node", "name": "w-1"}}),
            ),
            "binding",
            "CREATE",
        ),
    );
    assert_admitted(
        &p,
        "an eviction",
        &sub(create("", "pods", &named("x")), "eviction", "CREATE"),
    );
    assert_admitted(
        &p,
        "the CNI agent's endpoint record",
        &create(
            "cilium.io",
            "ciliumendpoints",
            &named("boss-node-trim-29852925-x7k2p"),
        ),
    );

    // After binding the pod names its node; the Job controller then
    // removes its finalizer — a metadata-only UPDATE.
    let mut bound = p0.clone();
    bound["spec"]["nodeName"] = json!("w-1");
    let mut finished = bound.clone();
    finished["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("finalizers");
    assert_admitted(
        &p,
        "a finalizer removed from the bound pod",
        &update("", "pods", &bound, &finished),
    );
    // A changed spec on a bound pod is judged, and the trim pod passes.
    let mut tolerant = bound.clone();
    tolerant["spec"]["tolerations"]
        .as_array_mut()
        .unwrap()
        .push(json!({"key": "x", "operator": "Exists"}));
    assert_admitted(
        &p,
        "a toleration added to the bound pod",
        &update("", "pods", &bound, &tolerant),
    );
    // WHEN it runs is not this policy's.
    let cj = cronjob_served();
    let mut moved = cj.clone();
    moved["spec"]["schedule"] = json!("10 11 * * *");
    assert_admitted(
        &p,
        "the schedule moved",
        &update("batch", "cronjobs", &cj, &moved),
    );
}

// ---- refused ----------------------------------------------------------------

#[test]
fn every_move_away_from_the_trim_pod_is_refused_on_every_carrier() {
    let p = policy();
    let mut refusing: BTreeSet<String> = BTreeSet::new();
    let moves = moves();
    assert!(moves.len() >= 60, "{} moves", moves.len());
    for (what, edit, naming) in &moves {
        for (label, group, resource, object, pointer) in carriers() {
            let mut moved = object.clone();
            edit(template(&mut moved, pointer));
            assert_ne!(
                moved, object,
                "{what}: a move that changes nothing tests nothing"
            );
            let said = assert_refused(
                &p,
                &format!("{what}, on a {label} at CREATE"),
                &create(group, resource, &moved),
                naming,
            );
            refusing.extend(said);
            // And as an UPDATE of the admitted object.
            if *what != "a node named at create" || resource != "pods" {
                assert_refused(
                    &p,
                    &format!("{what}, as an UPDATE of the {label}"),
                    &update(group, resource, &object, &moved),
                    naming,
                );
            }
        }
    }
    // Every pod-spec validation refused something above: none is dead.
    let pod_rules: BTreeSet<String> = p
        .validations
        .iter()
        .filter(|(e, _)| e.contains("variables.judged") || e.contains("apparmor"))
        .map(|(_, m)| m.clone())
        .collect();
    assert_eq!(
        pod_rules.len(),
        12,
        "the pod-spec validations: {pod_rules:#?}"
    );
    assert_eq!(
        pod_rules.difference(&refusing).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "validations no move above is refused by"
    );
}

#[test]
fn every_other_kind_and_subresource_is_refused() {
    let p = policy();
    const ONLY: &str = "admits only CronJob boss-node-trim";
    let named =
        |name: &str| json!({"metadata": {"name": name, "namespace": NAMESPACE}, "spec": {}});
    // A workload kind carrying the trim pod's own template is still not
    // the trim: only a CronJob, a Job and a Pod carry it.
    let tmpl = job()["spec"]["template"].clone();
    let wrapping = |name: &str| json!({"metadata": {"name": name}, "spec": {"template": tmpl}});
    let others: Vec<(&str, Request)> = vec![
        (
            "a Deployment",
            create("apps", "deployments", &wrapping("d")),
        ),
        ("a DaemonSet", create("apps", "daemonsets", &wrapping("d"))),
        (
            "a StatefulSet",
            create("apps", "statefulsets", &wrapping("d")),
        ),
        (
            "a ReplicaSet",
            create("apps", "replicasets", &wrapping("d")),
        ),
        (
            "a ReplicationController",
            create("", "replicationcontrollers", &wrapping("d")),
        ),
        (
            "a PodTemplate",
            create(
                "",
                "podtemplates",
                &json!({"metadata": {"name": "d"}, "template": tmpl}),
            ),
        ),
        ("a Secret", create("", "secrets", &named("s"))),
        (
            "a ConfigMap of another name",
            create("", "configmaps", &named("script")),
        ),
        (
            "a ServiceAccount of another name",
            create("", "serviceaccounts", &named("builder")),
        ),
        (
            "a token for the default ServiceAccount",
            sub(
                create(
                    "",
                    "serviceaccounts",
                    &json!({"spec": {"audiences": ["x"]}}),
                ),
                "token",
                "CREATE",
            ),
        ),
        (
            "a Role",
            create("rbac.authorization.k8s.io", "roles", &named("r")),
        ),
        (
            "a RoleBinding",
            create("rbac.authorization.k8s.io", "rolebindings", &named("r")),
        ),
        ("a Service", create("", "services", &named("s"))),
        (
            "a PersistentVolumeClaim",
            create("", "persistentvolumeclaims", &named("p")),
        ),
        ("a LimitRange", create("", "limitranges", &named("l"))),
        (
            "a CiliumNetworkPolicy",
            create("cilium.io", "ciliumnetworkpolicies", &named("allow")),
        ),
        (
            "a custom resource",
            create("longhorn.io", "volumes", &named("v")),
        ),
        ("a legacy Binding", create("", "bindings", &named("b"))),
        (
            "pods/exec",
            sub(
                create(
                    "",
                    "pods",
                    &json!({"kind": "PodExecOptions", "command": ["sh"], "container": "trim"}),
                ),
                "exec",
                "CONNECT",
            ),
        ),
        (
            "pods/attach",
            sub(
                create(
                    "",
                    "pods",
                    &json!({"kind": "PodAttachOptions", "container": "trim"}),
                ),
                "attach",
                "CONNECT",
            ),
        ),
        (
            "pods/portforward",
            sub(
                create("", "pods", &json!({"kind": "PodPortForwardOptions"})),
                "portforward",
                "CONNECT",
            ),
        ),
        (
            "pods/proxy",
            sub(
                create("", "pods", &json!({"kind": "PodProxyOptions"})),
                "proxy",
                "CONNECT",
            ),
        ),
        (
            "pods/ephemeralcontainers",
            sub(
                update("", "pods", &pod(), &pod()),
                "ephemeralcontainers",
                "UPDATE",
            ),
        ),
        (
            "pods/resize",
            sub(update("", "pods", &pod(), &pod()), "resize", "UPDATE"),
        ),
        (
            "jobs/scale",
            sub(update("batch", "jobs", &job(), &job()), "scale", "UPDATE"),
        ),
        (
            "a NetworkPolicy of another name",
            create("networking.k8s.io", "networkpolicies", &{
                let mut np = trim("NetworkPolicy");
                np["metadata"]["name"] = json!("allow-dns");
                np
            }),
        ),
    ];
    assert!(others.len() >= 25);
    for (what, request) in &others {
        assert_refused(&p, what, request, ONLY);
    }

    let mut second = cronjob_served();
    second["metadata"]["name"] = json!("boss-node-trim-2");
    assert_refused(
        &p,
        "the trim CronJob under a second name",
        &create("batch", "cronjobs", &second),
        "the one CronJob in boss-node-maintenance is boss-node-trim",
    );

    const NOTHING: &str = "selects every pod and allows nothing";
    let np = trim("NetworkPolicy");
    type NpEdit = fn(&mut Json);
    let opened: [(&str, NpEdit); 5] = [
        ("an egress rule", |n| n["spec"]["egress"] = json!([{}])),
        ("an ingress rule", |n| {
            n["spec"]["ingress"] = json!([{"from": [{"ipBlock": {"cidr": "0.0.0.0/0"}}]}])
        }),
        ("Egress left out of policyTypes", |n| {
            n["spec"]["policyTypes"] = json!(["Ingress"])
        }),
        ("a pod selector that selects nothing here", |n| {
            n["spec"]["podSelector"] = json!({"matchLabels": {"app": "nothing"}})
        }),
        ("no spec", |n| {
            n.as_object_mut().unwrap().remove("spec");
        }),
    ];
    for (what, edit) in opened {
        let mut moved = np.clone();
        edit(&mut moved);
        assert_refused(
            &p,
            &format!("deny-all with {what}"),
            &create("networking.k8s.io", "networkpolicies", &moved),
            NOTHING,
        );
        assert_refused(
            &p,
            &format!("deny-all UPDATED to {what}"),
            &update("networking.k8s.io", "networkpolicies", &np, &moved),
            NOTHING,
        );
    }

    // Every validation in the policy refused something in this file.
    assert_eq!(p.validations.len(), 15);
    let messages: BTreeSet<&String> = p.validations.iter().map(|(_, m)| m).collect();
    assert_eq!(messages.len(), 15, "each validation has its own message");
}

#[test]
fn an_update_is_judged_unless_it_leaves_the_spec_alone() {
    let p = policy();
    // An object admitted under an EARLIER policy (another digest): a
    // metadata-only write still passes, so it can finish and be collected…
    let mut earlier = pod();
    earlier["spec"]["containers"][0]["image"] = json!(
        "10.20.0.15:3000/david/alpine-k8s:1.32.0@sha256:1111111111111111111111111111111111111111111111111111111111111111"
    );
    let mut collected = earlier.clone();
    collected["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("finalizers");
    assert_admitted(
        &p,
        "an earlier pod losing its finalizer",
        &update("", "pods", &earlier, &collected),
    );
    // …and any write that MOVES its spec is judged as a new pod would be.
    let mut moved = earlier.clone();
    moved["spec"]["tolerations"] = json!([]);
    assert_refused(
        &p,
        "an earlier pod with its spec moved",
        &update("", "pods", &earlier, &moved),
        "pinned by digest",
    );
    // A Pod's annotations sit outside its spec, so they are judged on a
    // write that leaves the spec alone.
    let admitted = pod();
    let mut annotated = admitted.clone();
    annotated["metadata"]["annotations"] =
        json!({"container.apparmor.security.beta.kubernetes.io/trim": "unconfined"});
    assert_refused(
        &p,
        "an AppArmor annotation added by a metadata-only UPDATE",
        &update("", "pods", &admitted, &annotated),
        "AppArmor annotation",
    );
    // A nodeName may ride an UPDATE of a pod, and of nothing else.
    let mut named = job();
    named["spec"]["template"]["spec"]["nodeName"] = json!("cp-1");
    assert_refused(
        &p,
        "a Job template naming a node, by UPDATE",
        &update("batch", "jobs", &job(), &named),
        "pinned to the build node",
    );
    // CREATE carries no oldObject; `unchanged` must not read it.
    let mut r = create("", "pods", &pod());
    r.old = Json::Null;
    assert_admitted(&p, "a CREATE with a null oldObject", &r);
}

/// Two of the spellings the reviews found (a `--- # x` separator, quoted
/// and flow-style keys), as documents a smuggler would append to a
/// manifest. Decoded by the tree's YAML reader, each is an object like any
/// other, and is refused for what it is. The spellings that reader does
/// not decode (a lone CR or U+2028 line break, a backslash-continued
/// scalar, an explicit key, `!!binary`) decode on the server to the same
/// objects the moves above already carry.
#[test]
fn a_smuggled_document_is_refused_however_it_is_spelled() {
    let p = policy();
    let smuggled = r#"--- # x
apiVersion: v1
kind: Pod
metadata:
  name: helper
  namespace: boss-node-maintenance
spec: {"hostPID": true, containers: [{name: helper, image: busybox, "securityContext": {"privileged": true}, volumeMounts: [{name: root, mountPath: /host}]}], volumes: [{name: root, "hostPath": {path: /}}]}
--- # y
apiVersion: apps/v1
"kind": DaemonSet
metadata:
  name: helper
  namespace: boss-node-maintenance
spec: {template: {spec: {"hostNetwork": true, containers: [{name: helper, image: busybox, 'securityContext': {capabilities: {add: [SYS_ADMIN]}}}]}}}
--- # z
apiVersion: batch/v1
kind: CronJob
metadata:
  name: boss-node-trim
  namespace: boss-node-maintenance
spec: {schedule: "* * * * *", jobTemplate: {spec: {template: {spec: {restartPolicy: Never, containers: [{name: trim, image: busybox, "securityContext": {"privileged": true}}]}}}}}
"#;
    let objects = read_stream("a smuggled stream", smuggled).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(objects.len(), 3);
    let decoded: Vec<Json> = objects.iter().map(|o| typed(smuggled, &o.root)).collect();
    assert_eq!(decoded[0]["spec"]["volumes"][0]["hostPath"]["path"], "/");
    assert_eq!(
        decoded[0]["spec"]["containers"][0]["securityContext"]["privileged"],
        true
    );
    let said = assert_refused(
        &p,
        "a bare privileged Pod",
        &create("", "pods", &decoded[0]),
        "joins no host network, PID or IPC",
    );
    for naming in [
        "container securityContext is exactly",
        "mounts exactly two hostPath directories",
        "pinned by digest",
    ] {
        assert!(
            said.iter().any(|m| m.contains(naming)),
            "{naming:?} not among {said:?}"
        );
    }
    assert_refused(
        &p,
        "a DaemonSet",
        &create("apps", "daemonsets", &decoded[1]),
        "admits only CronJob boss-node-trim",
    );
    // The CronJob's own name does not carry another pod in.
    assert_refused(
        &p,
        "a CronJob named boss-node-trim with another pod",
        &create("batch", "cronjobs", &decoded[2]),
        "container securityContext is exactly",
    );
    assert_refused(
        &p,
        "the same, as an UPDATE of the trim CronJob",
        &update("batch", "cronjobs", &cronjob_served(), &decoded[2]),
        "pinned by digest",
    );
}

/// The fixture is what stands in for the trim manifest until it lands,
/// and then it is a second copy: held equal, or deleted. The Namespace
/// document the fixture carries (the trim car's head 0e13f736 declared
/// one) is not compared: the manifest that lands declares none.
#[test]
fn the_fixture_is_the_trim_manifest() {
    let workload = |path: &str| -> Vec<Json> {
        trim_docs(path)
            .into_iter()
            .filter(|d| d["kind"] != "Namespace")
            .collect()
    };
    let fixture = workload(TRIM_FIXTURE);
    let kinds: Vec<&str> = fixture.iter().filter_map(|d| d["kind"].as_str()).collect();
    assert_eq!(kinds, ["NetworkPolicy", "CronJob"]);
    if trim_in_tree() {
        assert_eq!(
            workload(TRIM),
            fixture,
            "{TRIM} and {TRIM_FIXTURE} differ: the manifest is no longer the workload the \
             policy was reviewed against. A car that means to move it changes the fixture \
             (or deletes it, and this test) beside the manifest, and says so"
        );
    }
}

// ---- what the server's compiler refuses and this evaluator does not ---------

/// The static type of a literal's element, as far as it can be told
/// without a type checker: a scalar literal's own type, `list<T>` and
/// `map<K,V>` for literals that are themselves of one type, `dyn` for a
/// `dyn(...)` call, and `expr` for anything else — a type this pin cannot
/// tell, so two of them in one literal are refused too.
fn literal_type(e: &cel::IdedExpr, found: &mut Vec<String>) -> String {
    use cel::common::ast::{EntryExpr, Expr, LiteralValue};
    let one_type = |what: &str, types: &[String], found: &mut Vec<String>| -> String {
        let distinct: BTreeSet<&String> = types.iter().collect();
        let untold = types.len() > 1 && types.iter().any(|t| t == "expr");
        if distinct.len() > 1 || untold {
            found.push(format!("a {what} of {types:?}"));
            "mixed".to_string()
        } else {
            types.first().cloned().unwrap_or_else(|| "?".to_string())
        }
    };
    match &e.expr {
        Expr::Literal(l) => match l {
            LiteralValue::Boolean(_) => "bool",
            LiteralValue::Bytes(_) => "bytes",
            LiteralValue::Double(_) => "double",
            LiteralValue::Int(_) => "int",
            LiteralValue::Null => "null",
            LiteralValue::String(_) => "string",
            LiteralValue::UInt(_) => "uint",
        }
        .to_string(),
        Expr::List(list) => {
            let types: Vec<String> = list
                .elements
                .iter()
                .map(|x| literal_type(x, found))
                .collect();
            format!("list<{}>", one_type("list literal", &types, found))
        }
        Expr::Map(map) => {
            let mut ks = Vec::new();
            let mut vs = Vec::new();
            for entry in &map.entries {
                match &entry.expr {
                    EntryExpr::MapEntry(kv) => {
                        ks.push(literal_type(&kv.key, found));
                        vs.push(literal_type(&kv.value, found));
                    }
                    EntryExpr::StructField(f) => vs.push(literal_type(&f.value, found)),
                }
            }
            let k = one_type("map literal with keys", &ks, found);
            let v = one_type("map literal with values", &vs, found);
            format!("map<{k},{v}>")
        }
        Expr::Call(call) => {
            if let Some(target) = &call.target {
                literal_type(target, found);
            }
            for arg in &call.args {
                literal_type(arg, found);
            }
            if call.func_name == "dyn" && call.target.is_none() {
                "dyn"
            } else {
                "expr"
            }
            .to_string()
        }
        Expr::Select(select) => {
            literal_type(&select.operand, found);
            "expr".to_string()
        }
        Expr::Comprehension(c) => {
            for part in [
                &c.iter_range,
                &c.accu_init,
                &c.loop_cond,
                &c.loop_step,
                &c.result,
            ] {
                literal_type(part, found);
            }
            "expr".to_string()
        }
        Expr::Struct(s) => {
            for entry in &s.entries {
                match &entry.expr {
                    EntryExpr::MapEntry(kv) => {
                        literal_type(&kv.key, found);
                        literal_type(&kv.value, found);
                    }
                    EntryExpr::StructField(f) => {
                        literal_type(&f.value, found);
                    }
                }
            }
            "expr".to_string()
        }
        Expr::Ident(_) | Expr::Unspecified => "expr".to_string(),
    }
}

/// Every list or map literal in `expr` whose elements are not of one type.
fn mixed_literals(expr: &str) -> Vec<String> {
    let program = Program::compile(expr).unwrap_or_else(|e| panic!("{expr}: {e}"));
    let mut found = Vec::new();
    literal_type(program.expression(), &mut found);
    found
}

/// THE API SERVER REFUSES TO COMPILE A LITERAL OF MIXED TYPES, and
/// cel-rust evaluates one without a word (review 9dc7696c, F1). The
/// Kubernetes CEL environment turns on HomogeneousAggregateLiterals:
/// `{'privileged': false, 'capabilities': {...}}` is "expected type 'bool'
/// but found 'map'", the policy is rejected when it is WRITTEN, and
/// because this file carries a Namespace the converge applies it first —
/// so a rejected policy would stop every converge at its first apply.
/// The first cut of this policy carried four such literals and this pin
/// and 73 mutants were green on it. So: no list or map literal in any
/// expression here mixes element types. The policy compares such objects
/// field by field under a key allow-list instead, and uses no `dyn(...)`.
#[test]
fn no_literal_in_the_policy_mixes_value_types() {
    // The rule itself, on shapes whose answer is known — the four the
    // review found, by shape, among them.
    for mixed in [
        "{'runAsNonRoot': false, 'runAsUser': 0}",
        "{'name': 'gate', 'hostPath': {'path': '/x'}}",
        "x == [{'name': 'gate', 'readOnly': true}]",
        "['a', 1]",
        "[{'a': 'x'}, {'a': 1}]",
        "{'a': 'x', 1: 'y'}",
        "[variables.a, variables.b]",
        "object.spec.all(k, k in ['a', 2])",
        "has(x.y) ? {'a': 1, 'b': '2'} : {}",
    ] {
        assert!(
            !mixed_literals(mixed).is_empty(),
            "the rule must refuse {mixed}"
        );
    }
    for one_type in [
        "{'drop': ['ALL'], 'add': ['SYS_ADMIN']}",
        "{'a': dyn(false), 'b': dyn(0)}",
        "[variables.script]",
        "x in ['a', 'b']",
        "{}",
        "[{'a': 'x'}, {'b': 'y'}]",
        "{'boss.dev/purpose': 'build'}",
    ] {
        assert_eq!(
            mixed_literals(one_type),
            Vec::<String>::new(),
            "the rule must pass {one_type}"
        );
    }
    let p = policy();
    let all = p
        .conditions
        .iter()
        .map(|c| ("matchCondition".to_string(), c))
        .chain(
            p.variables
                .iter()
                .map(|(n, e)| (format!("variable {n}"), e)),
        )
        .chain(
            p.validations
                .iter()
                .map(|(e, m)| (format!("validation {m:?}"), e)),
        );
    let mut found = Vec::new();
    let mut read = 0;
    for (what, expr) in all {
        read += 1;
        for literal in mixed_literals(expr) {
            found.push(format!("{what}: {literal}"));
        }
        assert!(
            !expr.contains("dyn("),
            "{what} uses dyn(...): compare field by field instead"
        );
    }
    assert!(read >= 25, "only {read} expressions were read");
    assert_eq!(
        found,
        Vec::<String>::new(),
        "the API server's CEL environment (HomogeneousAggregateLiterals) refuses to compile \
         these; compare the object field by field under a key allow-list"
    );
}

/// The matchCondition is the third confinement, so it must ANSWER for a
/// request the other two would have stopped: a cluster-scoped request
/// carries no namespace key at all, and reading an absent key is an
/// error, which failurePolicy Fail turns into a refusal — cluster-wide,
/// were `scope: Namespaced` ever dropped (review 9dc7696c, F2).
#[test]
fn a_request_with_no_namespace_is_not_judged_and_does_not_error() {
    let p = policy();
    let mut r = create("", "nodes", &json!({"metadata": {"name": "w-1"}}));
    r.namespace = "";
    for condition in &p.conditions {
        let request = json!({
            "resource": {"group": "", "version": "v1", "resource": "nodes"},
            "operation": "UPDATE",
        });
        let mut ctx = Context::default();
        ctx.add_variable("request", &request).expect("request");
        assert_eq!(
            eval(condition, &ctx).map(|v| format!("{v:?}")),
            Ok("Bool(false)".to_string()),
            "matchCondition {condition:?} on a request with no namespace"
        );
    }
    assert_eq!(judge(&p, &r), None);
}

/// A CONNECT may carry no object at all. It is refused by the first
/// validation ANSWERING false, never by a variable erroring on null.
#[test]
fn a_connect_with_no_object_is_refused_by_an_answer() {
    let p = policy();
    for subresource in ["exec", "attach", "portforward", "proxy"] {
        let r = sub(create("", "pods", &Json::Null), subresource, "CONNECT");
        assert_refused(
            &p,
            &format!("pods/{subresource} with a null object"),
            &r,
            "admits only CronJob boss-node-trim",
        );
    }
}
