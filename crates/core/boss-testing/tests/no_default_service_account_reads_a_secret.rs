//! No `default` ServiceAccount reaches a credential, no instance's
//! `default` ServiceAccount hands its token to the pods that run as it,
//! and only the boss pod runs as the account the credential broker is
//! bound to.
//!
//! WHY (backlog 28367b57, 2026-09-28). The adversarial review of the
//! per-act GitHub admin token (bf8726c9) found every one of the credential
//! broker's four RoleBindings naming ServiceAccount `boss:default` — the
//! identity of every pod in the namespace that declares none: the boss
//! Deployment, postgres, nats, nine CronJobs, and cloudflared, the pod that
//! faces the internet. Each of them mounted that SA's token, so each could
//! `get` (and `patch`) the two forge write tokens, the Cloudflare tunnel
//! secret and the DR push token the broker writes — and, once minted, an
//! org-admin GitHub token. The broker's k8s identity was `default` only
//! because boss.yaml named no ServiceAccount (the old comment on the
//! broker's first Role said so). The fix gives the boss pod its own SA,
//! binds the broker's Roles to THAT, and turns token automount off on the
//! `default` SA of every instance, so a pod that declares no identity
//! holds no credential at all.
//!
//! WHAT THE REVIEW OF THAT PIN FOUND (backlog cbb56130, 2026-09-29). Three
//! ways it stayed green on a real exposure, each now a mutation below that
//! must go red:
//!   * nothing held WHO ELSE runs as the broker's account — the exposure
//!     had moved from "any pod as `default`" to "any pod that names `boss`",
//!     and `serviceAccountName: boss` on the files-gc CronJob was green
//!     (M11). That account now holds the org-admin token (0660454c), so
//!     `only_the_boss_deployment_runs_as_the_broker_bound_service_account`
//!     holds the set to ONE member in every instance namespace;
//!   * its reader passed an inline `subjects:` / `rules:` and a
//!     `kind: List` (M2b, M6, M9) — the reader is now the shared
//!     `boss_testing::rbac`, which refuses every shape it does not read;
//!   * it counted only get/list/watch on secrets, so a Role that could
//!     only OVERWRITE a broker Secret, or `create` a pod or a
//!     `serviceaccounts/token` (either one becomes `boss`'s token), was
//!     green (M4, M5). The deny set is `rbac::credential_reach`, the same
//!     one the dev-session pin uses.
//!
//! WHAT IS READ. `rbac::estate()`: `render-instance.sh --all` — exactly
//! what the converge applies, in every namespace it applies it — plus
//! every other manifest under `infra/`.
//!
//! tree-wide pin — it reads every manifest under infra/, which no
//! changed-file map attributes to this crate, so every scoped gate runs
//! it whatever its scope (`tree_wide_pins` in infra/gate.sh).

use boss_testing::rbac::{
    self, Estate, Judgement, Object, Reach, credential_grants, credential_reach, reach,
    read_stream, yaml_bool,
};
use std::collections::BTreeSet;

const WHY: &str = "a `default` ServiceAccount is every pod that declares no identity, and \
                   none may reach a credential (backlog 28367b57) — bind the Role to the \
                   workload's own ServiceAccount";

fn estate_objects() -> Vec<Object> {
    rbac::estate().objects().unwrap_or_else(|e| panic!("{e}"))
}

/// Every binding that gives any `default` ServiceAccount a rule reaching a
/// credential, or a role no manifest declares.
fn default_grants(objects: &[Object]) -> Result<Judgement, String> {
    credential_grants(objects, |r| r.includes(None, "default"), |_| true, WHY)
}

/// The boss Deployment of one instance namespace, and the account it runs as.
fn boss_pod(ns: &str, objects: &[Object]) -> Result<(String, String), String> {
    let boss = objects
        .iter()
        .find(|o| o.kind == "Deployment" && o.name == "boss")
        .ok_or_else(|| format!("{ns}: the render carries no boss Deployment"))?;
    let sa = boss
        .service_account()
        .ok_or_else(|| format!("{}: no pod spec", boss.label()))?;
    Ok((boss.label(), sa))
}

fn automount_findings(e: &Estate) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for ns in &e.instances {
        let objects = e.objects_in(ns)?;
        let default_sa = objects.iter().find(|o| {
            o.kind == "ServiceAccount"
                && o.name == "default"
                && o.namespace.as_deref() == Some(ns.as_str())
        });
        match default_sa {
            None => out.push(format!(
                "instance namespace {ns}: its render declares no ServiceAccount `default` — so \
                 its automount is the cluster's default, ON, and every pod that names no \
                 identity carries an API token (backlog 28367b57). Declare it in an \
                 `instance` manifest (boss.yaml) so every instance gets one."
            )),
            Some(sa)
                if sa
                    .root
                    .get("automountServiceAccountToken")
                    .and_then(yaml_bool)
                    != Some(false) =>
            {
                out.push(format!(
                    "{}: must declare automountServiceAccountToken: false",
                    sa.label()
                ));
            }
            Some(_) => {}
        }
        // A pod-level true beats the SA's false — in any spelling the API
        // server's YAML reads as true (`yes`, `on`), and a value it may not
        // read as a boolean at all is not a false.
        out.extend(
            objects
                .iter()
                .filter(|o| o.service_account().as_deref() == Some("default"))
                .filter(|o| {
                    o.pod_spec()
                        .and_then(|s| s.get("automountServiceAccountToken"))
                        .is_some_and(|n| yaml_bool(n) != Some(false))
                })
                .map(|o| {
                    format!(
                        "{} runs as `default` and sets automountServiceAccountToken: true",
                        o.label()
                    )
                }),
        );
    }
    Ok(out)
}

/// The broker's RoleBindings, wherever they live.
fn broker_bindings(objects: &[Object]) -> Vec<&Object> {
    objects
        .iter()
        .filter(|o| o.kind == "RoleBinding" && o.name.starts_with("credential-broker-"))
        .collect()
}

/// Two copies of one fact (CLAUDE.md §9a): the SA the boss Deployment runs
/// as, and the subject of every broker binding. Drift strands the broker —
/// its Secret reads answer 403 — with nothing else changed.
fn broker_binding_findings(e: &Estate) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for ns in &e.instances {
        let objects = e.objects_in(ns)?;
        let (label, sa) = boss_pod(ns, &objects)?;
        if sa == "default" {
            out.push(format!(
                "{label}: the broker's pod runs as `default`, not its own identity"
            ));
        } else if !objects.iter().any(|o| {
            o.kind == "ServiceAccount"
                && o.name == sa
                && o.namespace.as_deref() == Some(ns.as_str())
        }) {
            out.push(format!(
                "{ns}: the boss Deployment runs as ServiceAccount `{sa}`, which this instance's \
                 render does not declare — the pod would be refused. Declare it in boss.yaml."
            ));
        }
    }
    // The broker is a pipeline manifest: its bindings exist once, in the
    // source's set, and name the source instance's boss pod.
    let source = e.objects_in(&e.source)?;
    let (_, boss_sa) = boss_pod(&e.source, &source)?;
    let bindings = broker_bindings(&source);
    if bindings.len() < 4 {
        out.push(format!(
            "the broker's four RoleBindings are read in {} (saw {}) — a reader that sees none \
             proves nothing",
            e.source,
            bindings.len()
        ));
    }
    let want = Reach::Account {
        namespace: e.source.clone(),
        name: boss_sa.clone(),
    };
    for b in bindings {
        let reached: Vec<Reach> = b
            .subjects()?
            .iter()
            .map(|s| reach(s, b.namespace.as_deref()))
            .collect();
        if reached != vec![want.clone()] {
            out.push(format!(
                "{}: its one subject must be the boss pod's ServiceAccount {}/{boss_sa}, and it \
                 reaches {reached:?}",
                b.label(),
                e.source
            ));
        }
    }
    Ok(out)
}

/// In every instance namespace, the ONLY object that names the broker-bound
/// account as a pod's identity is Deployment/boss. Every other pod that
/// names it mounts a token that can read and patch every broker Secret —
/// since 0660454c, the org-admin GitHub token among them.
fn only_boss_findings(e: &Estate) -> Result<Vec<String>, String> {
    let all = e.objects()?;
    let broker_bound: Vec<(String, String)> = broker_bindings(&all)
        .into_iter()
        .map(|b| Ok((b, b.subjects()?)))
        .collect::<Result<Vec<_>, String>>()?
        .into_iter()
        .flat_map(|(b, subjects)| {
            subjects
                .iter()
                .filter_map(|s| match reach(s, b.namespace.as_deref()) {
                    Reach::Account { namespace, name } => Some((namespace, name)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let mut out = Vec::new();
    for ns in &e.instances {
        let (_, boss_sa) = boss_pod(ns, &e.objects_in(ns)?)?;
        let guarded: BTreeSet<&str> = broker_bound
            .iter()
            .filter(|(n, _)| n == ns)
            .map(|(_, name)| name.as_str())
            .chain([boss_sa.as_str()])
            .collect();
        let in_ns = |o: &&Object| match o.namespace.as_deref() {
            Some(n) => n == ns,
            None => o.source.starts_with(&format!("{ns}/")),
        };
        let mut boss_seen = false;
        for o in all.iter().filter(in_ns) {
            // A token Secret annotated with the account IS its token, minted
            // by the controller and readable by whoever reads that Secret —
            // the account run without a pod at all.
            let token_for = (o.kind == "Secret")
                .then(|| {
                    o.root
                        .path(&[
                            "metadata",
                            "annotations",
                            "kubernetes.io/service-account.name",
                        ])
                        .and_then(|n| n.str())
                        .map(str::to_string)
                })
                .flatten();
            let named: Vec<String> = o
                .names_service_accounts()
                .into_iter()
                .chain(token_for)
                .filter(|sa| guarded.contains(sa.as_str()))
                .collect();
            if named.is_empty() {
                continue;
            }
            if o.kind == "Deployment" && o.name == "boss" {
                boss_seen = true;
                continue;
            }
            out.push(format!(
                "{} runs as ServiceAccount {ns}/{} — the account the credential broker is bound \
                 to, whose token reads and patches every broker Secret (the org-admin GitHub \
                 token among them). Only Deployment/boss may run as it; give this workload its \
                 own ServiceAccount (backlog cbb56130).",
                o.label(),
                named.join(", ")
            ));
        }
        if !boss_seen {
            out.push(format!(
                "{ns}: no Deployment/boss was seen naming {guarded:?} — the reader is blind, \
                 not the namespace clean"
            ));
        }
    }
    Ok(out)
}

/// Every check this pin makes, over one estate.
fn every_finding(e: &Estate) -> Result<Vec<String>, String> {
    let mut out = default_grants(&e.objects()?)?.findings;
    out.extend(automount_findings(e)?);
    out.extend(broker_binding_findings(e)?);
    out.extend(only_boss_findings(e)?);
    Ok(out)
}

#[test]
fn no_binding_in_the_render_gives_a_default_service_account_a_secret_read() {
    let all = estate_objects();
    // A control: the reader sees the broker's Roles as Secret readers, or
    // an empty verdict below would be the reader failing, not the tree
    // passing.
    let broker_roles = all
        .iter()
        .filter(|o| o.kind == "Role" && o.name.starts_with("credential-broker-"))
        .filter(|o| {
            o.rules()
                .unwrap_or_else(|e| panic!("{e}"))
                .iter()
                .any(|r| credential_reach(r).iter().any(|c| c.starts_with("secrets")))
        })
        .count();
    assert!(
        broker_roles >= 4,
        "the reader must see the broker's four Secret-reading Roles (saw {broker_roles}) — \
         a reader that sees none proves nothing"
    );
    let found = default_grants(&all).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        found.findings.is_empty(),
        "a `default` ServiceAccount reaches a credential:\n  {}",
        found.findings.join("\n  ")
    );
}

#[test]
fn every_instance_turns_token_automount_off_on_its_default_service_account() {
    let found = automount_findings(rbac::estate()).unwrap_or_else(|e| panic!("{e}"));
    assert!(found.is_empty(), "{}", found.join("\n  "));
}

#[test]
fn the_broker_is_bound_to_the_service_account_the_boss_pod_runs_as() {
    let found = broker_binding_findings(rbac::estate()).unwrap_or_else(|e| panic!("{e}"));
    assert!(found.is_empty(), "{}", found.join("\n  "));
}

#[test]
fn only_the_boss_deployment_runs_as_the_broker_bound_service_account() {
    let found = only_boss_findings(rbac::estate()).unwrap_or_else(|e| panic!("{e}"));
    assert!(found.is_empty(), "\n  {}", found.join("\n  "));
}

#[test]
fn the_internet_facing_connector_mounts_no_token() {
    // cloudflared reads its credentials through a Secret VOLUME (the
    // kubelet's read, not the pod's) and calls no API; it is the pod that
    // faces the internet, so it says so itself rather than inheriting it.
    let all = estate_objects();
    let d = all
        .iter()
        .find(|o| o.kind == "Deployment" && o.name == "cloudflared")
        .expect("the cloudflared Deployment");
    assert_eq!(
        d.pod_spec()
            .and_then(|s| s.get("automountServiceAccountToken"))
            .and_then(yaml_bool),
        Some(false),
        "{}: must declare automountServiceAccountToken: false",
        d.label()
    );
}

// ---- the reader catches what it claims to catch ---------------------------

const FIXTURE: &str = r#"
kind: Role
metadata:
  name: reads-one
  namespace: ns
rules:
  - apiGroups: [""]
    resources: [secrets]
    resourceNames: [one]
    verbs: [get, patch]
---
kind: Role
metadata:
  name: reads-pods
  namespace: ns
rules:
  - apiGroups: [""]
    resources: ["pods"]
    verbs: ["get", "list"]
---
kind: ClusterRole
metadata:
  name: lists-everything
rules:
  - apiGroups:
      - "*"
    resources:
      - "*"
    verbs:
      - list
---
kind: RoleBinding
metadata:
  name: to-default
  namespace: ns
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: reads-one
subjects:
  - kind: ServiceAccount
    name: default
    namespace: ns
---
kind: RoleBinding
metadata:
  name: to-own-sa
  namespace: ns
roleRef:
  kind: Role
  name: reads-one
subjects:
  - kind: ServiceAccount
    name: worker
    namespace: ns
---
kind: RoleBinding
metadata:
  name: pods-to-default
  namespace: ns
roleRef:
  kind: Role
  name: reads-pods
subjects:
  - kind: ServiceAccount
    name: default
---
kind: ClusterRoleBinding
metadata:
  name: block-list-to-group
roleRef:
  kind: ClusterRole
  name: lists-everything
subjects:
  - kind: Group
    name: system:serviceaccounts:ns
---
kind: RoleBinding
metadata:
  name: builtin-to-default
  namespace: ns
roleRef:
  kind: ClusterRole
  name: edit
subjects:
  - kind: User
    name: system:serviceaccount:ns:default
"#;

#[test]
fn the_reader_names_every_binding_that_hands_default_a_secret_read() {
    let docs = read_stream("fixture.yaml", FIXTURE).unwrap_or_else(|e| panic!("{e}"));
    let found = default_grants(&docs)
        .unwrap_or_else(|e| panic!("{e}"))
        .findings;
    let hit = |name: &str| found.iter().any(|f| f.contains(&format!("/{name} (")));
    assert!(hit("to-default"), "a direct binding is named: {found:?}");
    assert!(
        hit("block-list-to-group"),
        "a block-list wildcard role through the SA group is named: {found:?}"
    );
    assert!(
        hit("builtin-to-default"),
        "a role the set does not declare is named, not passed: {found:?}"
    );
    assert!(
        !hit("to-own-sa"),
        "a binding to a workload's own SA is fine: {found:?}"
    );
    assert!(
        !hit("pods-to-default"),
        "a read of no credential is not this pin's business: {found:?}"
    );
    assert_eq!(found.len(), 3, "{found:#?}");
}

#[test]
#[should_panic(expected = "refused, not passed")]
fn the_reader_refuses_a_subject_it_cannot_read() {
    read_stream(
        "fixture.yaml",
        "kind: RoleBinding\nmetadata:\n  name: odd\n  namespace: ns\nroleRef:\n  kind: Role\n  name: r\nsubjects:\n  - kind: ServiceAccount\n    weird line\n",
    )
    .unwrap_or_else(|e| panic!("{e}"));
}

// ---- every mutation the reviews ran goes red ------------------------------

/// A binding of `subject_yaml` (the lines under `subjects:`) to `role` in
/// namespace boss, preceded by `role_yaml` when the role is new.
fn bound(role_yaml: &str, role_kind: &str, role: &str, subjects: &str) -> String {
    format!(
        "{role_yaml}---\napiVersion: rbac.authorization.k8s.io/v1\nkind: RoleBinding\nmetadata:\n  name: mutation\n  namespace: boss\nroleRef:\n  apiGroup: rbac.authorization.k8s.io\n  kind: {role_kind}\n  name: {role}\nsubjects:\n{subjects}"
    )
}

const TO_DEFAULT: &str = "  - kind: ServiceAccount\n    name: default\n    namespace: boss\n";

fn role(rules: &str) -> String {
    format!(
        "apiVersion: rbac.authorization.k8s.io/v1\nkind: Role\nmetadata:\n  name: mutant\n  namespace: boss\nrules:\n{rules}"
    )
}

#[test]
fn every_review_mutation_goes_red() {
    let e = rbac::estate();
    assert_eq!(
        every_finding(e),
        Ok(Vec::new()),
        "the control: the unmutated estate passes every check"
    );
    let broker = "credential-broker-github-app";
    let mutations: Vec<(&str, Estate)> = vec![
        // Review 28367b57 (car f25c8082): the ones it found green.
        (
            "M2b inline subjects",
            e.with_file(
                &e.source,
                "m.yaml",
                &format!(
                    "apiVersion: rbac.authorization.k8s.io/v1\nkind: RoleBinding\nmetadata:\n  name: m\n  namespace: boss\nroleRef:\n  kind: Role\n  name: {broker}\nsubjects: [{{kind: ServiceAccount, name: default, namespace: boss}}]\n"
                ),
            ),
        ),
        (
            "M4 write-only verbs on secrets",
            e.with_file(
                &e.source,
                "m.yaml",
                &bound(
                    &role("  - apiGroups: [\"\"]\n    resources: [secrets]\n    verbs: [update, patch, create, delete]\n"),
                    "Role",
                    "mutant",
                    TO_DEFAULT,
                ),
            ),
        ),
        (
            "M5a create pods",
            e.with_file(
                &e.source,
                "m.yaml",
                &bound(
                    &role("  - apiGroups: [\"\"]\n    resources: [pods]\n    verbs: [create]\n"),
                    "Role",
                    "mutant",
                    TO_DEFAULT,
                ),
            ),
        ),
        (
            "M5b create serviceaccounts/token",
            e.with_file(
                &e.source,
                "m.yaml",
                &bound(
                    &role("  - apiGroups: [\"\"]\n    resources: [serviceaccounts/token]\n    verbs: [create]\n"),
                    "Role",
                    "mutant",
                    TO_DEFAULT,
                ),
            ),
        ),
        (
            "M6 inline rules",
            e.with_file(
                &e.source,
                "m.yaml",
                &bound(
                    "apiVersion: rbac.authorization.k8s.io/v1\nkind: Role\nmetadata:\n  name: mutant\n  namespace: boss\nrules: [{apiGroups: [\"\"], resources: [secrets], verbs: [get]}]\n",
                    "Role",
                    "mutant",
                    TO_DEFAULT,
                ),
            ),
        ),
        (
            "M9 kind: List",
            e.with_file(
                &e.source,
                "m.yaml",
                &format!(
                    "apiVersion: v1\nkind: List\nitems:\n  - apiVersion: rbac.authorization.k8s.io/v1\n    kind: RoleBinding\n    metadata:\n      name: m\n      namespace: boss\n    roleRef:\n      kind: Role\n      name: {broker}\n    subjects:\n      - kind: ServiceAccount\n        name: default\n        namespace: boss\n"
                ),
            ),
        ),
        (
            "M11 the files-gc CronJob runs as boss",
            e.edited(
                &format!("{}/boss-files-gc.yaml", e.source),
                "        spec:\n          # The uid this workload runs as",
                "        spec:\n          serviceAccountName: boss\n          # The uid this workload runs as",
            ),
        ),
        // Review 28367b57: the ones it found red, kept red.
        (
            "M1 a broker binding back on default",
            e.edited(
                &format!("{}/boss-credential-broker.yaml", e.source),
                &format!("  name: {broker}\nsubjects:\n  - kind: ServiceAccount\n    name: boss\n"),
                &format!("  name: {broker}\nsubjects:\n  - kind: ServiceAccount\n    name: default\n"),
            ),
        ),
        (
            "M3 an undeclared role to system:authenticated",
            e.with_file(
                &e.source,
                "m.yaml",
                "apiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRoleBinding\nmetadata:\n  name: m\nroleRef:\n  kind: ClusterRole\n  name: edit\nsubjects:\n  - kind: Group\n    name: system:authenticated\n",
            ),
        ),
        (
            "M8 default automount on",
            e.edited(
                &format!("{}/boss.yaml", e.source),
                "  name: default\n  namespace: boss\nautomountServiceAccountToken: false",
                "  name: default\n  namespace: boss\nautomountServiceAccountToken: true",
            ),
        ),
        (
            "M10 a CronJob with no SA asks for the token back",
            e.with_file(
                &e.source,
                "m.yaml",
                "apiVersion: batch/v1\nkind: CronJob\nmetadata:\n  name: m\n  namespace: boss\nspec:\n  schedule: '* * * * *'\n  jobTemplate:\n    spec:\n      template:\n        spec:\n          automountServiceAccountToken: true\n          containers:\n            - name: c\n              image: x\n",
            ),
        ),
        (
            "M12 cloudflared runs as boss",
            e.edited(
                &format!("{}/cloudflared.yaml", e.source),
                "      automountServiceAccountToken: false\n",
                "      serviceAccountName: boss\n      automountServiceAccountToken: true\n",
            ),
        ),
        // Shapes the new reader reads, each a way the old one was blind.
        (
            "a User subject spelling the default SA",
            e.with_file(
                &e.source,
                "m.yaml",
                &bound("", "Role", broker, "  - kind: User\n    name: system:serviceaccount:boss:default\n"),
            ),
        ),
        (
            "a single-quoted namespace",
            e.with_file(
                &e.source,
                "m.yaml",
                &bound("", "Role", broker, TO_DEFAULT).replace("  namespace: boss\nroleRef", "  namespace: 'boss'\nroleRef"),
            ),
        ),
        (
            "the deprecated serviceAccount field on a Pod",
            e.with_file(
                &e.source,
                "m.yaml",
                "apiVersion: v1\nkind: Pod\nmetadata:\n  name: m\n  namespace: boss\nspec:\n  serviceAccount: boss\n  containers:\n    - name: c\n      image: x\n",
            ),
        ),
        (
            "a playground chore runs as the playground's boss account",
            e.with_file(
                "boss-playground",
                "m.yaml",
                "apiVersion: batch/v1\nkind: Job\nmetadata:\n  name: m\n  namespace: boss-playground\nspec:\n  template:\n    spec:\n      serviceAccountName: boss\n      containers:\n        - name: c\n          image: x\n",
            ),
        ),
        (
            "a pod-level `yes` asks for the token back",
            e.with_file(
                &e.source,
                "m.yaml",
                "apiVersion: batch/v1\nkind: Job\nmetadata:\n  name: m\n  namespace: boss\nspec:\n  template:\n    spec:\n      automountServiceAccountToken: yes\n      containers:\n        - name: c\n          image: x\n",
            ),
        ),
        (
            "a token Secret minted for the broker's account",
            e.with_file(
                &e.source,
                "m.yaml",
                "apiVersion: v1\nkind: Secret\nmetadata:\n  name: m\n  namespace: boss\n  annotations:\n    kubernetes.io/service-account.name: boss\ntype: kubernetes.io/service-account-token\n",
            ),
        ),
        (
            "a byte-order mark before the kind",
            e.with_file(
                &e.source,
                "m.yaml",
                &format!("\u{feff}{}", bound("", "Role", broker, TO_DEFAULT).trim_start_matches("---\n")),
            ),
        ),
        (
            "a flow-mapping document",
            e.with_file(
                &e.source,
                "m.yaml",
                &format!("{{apiVersion: rbac.authorization.k8s.io/v1, kind: RoleBinding, metadata: {{name: m, namespace: boss}}, roleRef: {{kind: Role, name: {broker}}}}}\n"),
            ),
        ),
        // Review cbb5 (run 42ee3f9b): the kubelet authorizes a WebSocket
        // exec, attach or port-forward — opened as a GET — as `get
        // nodes/proxy`, so that read is an exec into the boss pod, whose
        // env carries the broker's root tokens.
        (
            "get nodes/proxy for default",
            e.with_file(
                &e.source,
                "m.yaml",
                "apiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRole\nmetadata:\n  name: mutant\nrules:\n  - apiGroups: [\"\"]\n    resources: [nodes/proxy]\n    verbs: [get]\n---\napiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRoleBinding\nmetadata:\n  name: mutant\nroleRef:\n  kind: ClusterRole\n  name: mutant\nsubjects:\n  - kind: ServiceAccount\n    name: default\n    namespace: boss\n",
            ),
        ),
    ];
    let green: Vec<&str> = mutations
        .iter()
        .filter(|(name, m)| {
            let verdict = every_finding(m);
            // Each verdict's reason, for the reviewer who runs --nocapture:
            // red for the RIGHT reason is the claim, not red at all.
            eprintln!("MUTATION {name}: {verdict:?}");
            matches!(verdict, Ok(f) if f.is_empty())
        })
        .map(|(name, _)| *name)
        .collect();
    assert!(
        green.is_empty(),
        "these mutations pass the pin — each is a real exposure it must refuse: {green:?}"
    );
    // The other control: an added file that reaches no credential stays
    // green, so the reds above are the mutations, not the harness.
    let harmless = e.with_file(
        &e.source,
        "m.yaml",
        &bound(
            &role("  - apiGroups: [\"\"]\n    resources: [pods, pods/log]\n    verbs: [get, list, watch]\n"),
            "Role",
            "mutant",
            TO_DEFAULT,
        ),
    );
    assert_eq!(every_finding(&harmless), Ok(Vec::new()));
}
