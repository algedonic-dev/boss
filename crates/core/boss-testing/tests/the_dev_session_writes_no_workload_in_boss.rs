//! No Role bound to the dev session may WRITE a workload or a pod — or
//! reach any other credential — in an instance namespace (backlog
//! 978bbd7d; widened by cbb56130).
//!
//! WHY A WRITE IS A READ HERE. RBAC scopes `patch` to an object, never
//! to a field. A patch on a Deployment's pod template is admitted, and
//! the ReplicaSet controller — not the patcher — creates the new pod, so
//! the patcher never needs `create pods` or `get secrets` to have a pod
//! start with any Secret the namespace holds as env or a volume, and
//! `pods/log` hands it back. `deploy/boss` already names the credential
//! broker's root tokens (forgejo admin, cloudflare, stripe) in its own
//! env, so on 2026-09-29 no new reference was even needed: change the
//! container's command and read the log. That was the grant
//! `dev-session-ops-read` carried from 2026-09-02 (train #181) under a
//! comment saying "No secrets access; the credential boundary stays
//! exactly where it was", measured live from the dev pod by agent-run
//! 6beb7d1a: `kubectl auth can-i patch deployment/boss -n boss` = yes.
//! It served one hand-run `rollout undo` in the emergency-merge
//! protocol, which the `rollback-to` ops verb and the forge's watchdog
//! both do by a NAMED build from outside the session.
//!
//! WHAT THIS HOLDS. Every RoleBinding in an instance namespace, and every
//! ClusterRoleBinding, that reaches the `boss-dev/dev-session`
//! ServiceAccount — by name, as the user `system:serviceaccount:boss-dev:
//! dev-session`, or through a group it is a member of — is resolved to the
//! Role it grants, and no rule of that Role may reach a credential:
//! `rbac::credential_reach`, the ONE deny set the no-default-SA pin uses
//! too. That is a write on a pod or any pod-template owner
//! (replicationcontrollers included — its absence was one of the review's
//! bypasses), any verb on secrets, `create serviceaccounts/token` (it
//! could mint `boss/boss`, the broker's account), impersonate, bind,
//! escalate, any write on an RBAC object — and, since review cbb5, any
//! verb on `nodes/proxy`, CSR approval, and webhook-configuration writes,
//! and since its release re-review (cbdf1e9c) `create` on a node-client
//! CSR subresource and writes on mutating admission policies. `resourceNames` exempt
//! nothing: a name-scoped patch on `boss` was exactly the defect. A
//! binding whose Role no manifest declares is refused as well, because
//! its rules cannot be read.
//!
//! WHAT THE REVIEW OF THIS PIN FOUND (review 978b, 2026-09-29): 11 of 15
//! probes passed it — inline rules, subjects and metadata, a single-quoted
//! namespace, `kind: List`, an aggregated ClusterRole, a `User` subject
//! naming the account, replicationcontrollers, and no deny set beyond pod
//! writes. Each is a mutation below that must go red, and the reader is
//! the shared `boss_testing::rbac`, which refuses every shape it does not
//! read rather than reading it as empty.
//!
//! The manifests are `rbac::estate()`: the render the converge applies,
//! in every instance namespace, plus every other manifest under `infra/`,
//! so a grant cannot hide by moving to another file.
//!
//! tree-wide pin — it reads every manifest under infra/, which no
//! changed-file map attributes to this crate, so every scoped gate runs
//! it whatever its scope (`tree_wide_pins` in infra/gate.sh).

use boss_testing::rbac::{self, Estate, Judgement, Object, credential_grants, read_stream};

const SESSION_NAMESPACE: &str = "boss-dev";
const SESSION_ACCOUNT: &str = "dev-session";

const WHY: &str = "a pod-template write is a read of every Secret the template names, and \
                   the rest of the set is a credential by another door (978bbd7d, cbb56130)";

/// Every way the objects let the session reach a credential in one of the
/// `guarded` namespaces (a ClusterRoleBinding reaches them all).
fn findings(objects: &[Object], guarded: &[String]) -> Result<Judgement, String> {
    credential_grants(
        objects,
        |r| r.includes(Some(SESSION_NAMESPACE), SESSION_ACCOUNT),
        |scope| scope.is_none_or(|ns| guarded.iter().any(|g| g == ns)),
        WHY,
    )
}

fn in_boss(objects: &[Object]) -> Result<Judgement, String> {
    findings(objects, &["boss".to_string()])
}

fn estate_findings(e: &Estate) -> Result<Judgement, String> {
    findings(&e.objects()?, &e.instances)
}

#[test]
fn no_role_bound_to_the_dev_session_writes_a_pod_in_boss() {
    let e = rbac::estate();
    let objects = e.objects().unwrap_or_else(|err| panic!("{err}"));
    let control = in_boss(&objects).unwrap_or_else(|err| panic!("{err}"));
    assert!(
        control.bindings > 0,
        "no binding of {SESSION_NAMESPACE}/{SESSION_ACCOUNT} reaches namespace boss \
         (dev-session-ops-read should) — the reader is blind, not the grant clean"
    );
    let found = estate_findings(e).unwrap_or_else(|err| panic!("{err}"));
    assert!(
        found.findings.is_empty(),
        "the dev session reaches a credential in an instance namespace {:?}:\n  {}",
        e.instances,
        found.findings.join("\n  ")
    );
}

/// The shape `dev-session-ops-read` had on origin/main from 2026-09-02
/// until 978bbd7d: a read-only Role with one name-scoped patch.
const THE_FILED_SHAPE: &str = r#"# header comment
---
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: dev-session-ops-read
  namespace: boss
rules:
  - apiGroups: [""]
    resources: [pods, pods/log, events, services]
    verbs: [get, list, watch]
  - apiGroups: [apps]
    resources: [deployments]
    resourceNames: [boss]
    verbs: [patch]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: dev-session-ops-read
  namespace: boss
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: dev-session-ops-read
subjects:
  - kind: ServiceAccount
    name: dev-session
    namespace: boss-dev
"#;

fn read(text: &str) -> Vec<Object> {
    read_stream("fixture.yaml", text).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn the_check_names_the_rule_it_refuses() {
    let found = in_boss(&read(THE_FILED_SHAPE)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(found.bindings, 1);
    assert_eq!(found.findings.len(), 1, "{:#?}", found.findings);
    for needle in [
        "fixture.yaml:12:",
        "Role dev-session-ops-read",
        "\"patch\"",
        "apps/deployments",
    ] {
        assert!(
            found.findings[0].contains(needle),
            "{needle} missing from: {}",
            found.findings[0]
        );
    }
}

#[test]
fn the_same_rule_bound_elsewhere_is_not_the_sessions() {
    // Controls: the reader keys on who is bound and where, so the same
    // Role bound to another account, or bound in the session's own
    // namespace, is not this check's business.
    let other_account = THE_FILED_SHAPE.replace("    name: dev-session\n", "    name: other\n");
    let own_namespace = THE_FILED_SHAPE.replace("namespace: boss\n", "namespace: boss-dev\n");
    for text in [other_account, own_namespace] {
        assert_eq!(in_boss(&read(&text)), Ok(Judgement::default()));
    }
}

#[test]
fn a_group_binding_a_wildcard_and_a_subresource_all_count() {
    let text = r#"kind: ClusterRole
metadata:
  name: wide
rules:
  - apiGroups: ["*"]
    resources: ["*"]
    verbs: ["*"]
---
kind: Role
metadata:
  name: execer
  namespace: boss
rules:
  - apiGroups: [""]
    resources: [pods/exec]
    verbs: [create]
---
kind: ClusterRoleBinding
metadata:
  name: everyone
roleRef:
  kind: ClusterRole
  name: wide
subjects:
  - kind: Group
    name: system:serviceaccounts:boss-dev
---
kind: RoleBinding
metadata:
  name: exec
  namespace: boss
roleRef:
  kind: Role
  name: execer
subjects:
  - kind: ServiceAccount
    name: dev-session
    namespace: boss-dev
"#;
    let found = in_boss(&read(text)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(found.bindings, 2);
    assert_eq!(found.findings.len(), 2, "{:#?}", found.findings);
    assert!(
        found.findings[0].contains("ClusterRole wide"),
        "{}",
        found.findings[0]
    );
    assert!(
        found.findings[1].contains("pods/exec"),
        "{}",
        found.findings[1]
    );
}

#[test]
fn an_undeclared_role_and_an_unreadable_rule_are_refused_not_passed() {
    let undeclared = r#"kind: RoleBinding
metadata:
  name: edit
  namespace: boss
roleRef:
  kind: ClusterRole
  name: edit
subjects:
  - kind: ServiceAccount
    name: dev-session
    namespace: boss-dev
"#;
    let found = in_boss(&read(undeclared)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(found.findings.len(), 1, "{:#?}", found.findings);
    assert!(
        found.findings[0].contains("no manifest declares"),
        "{}",
        found.findings[0]
    );

    // A block list is read now, not refused — and caught.
    let binding = "---\nkind: RoleBinding\nmetadata:\n  name: b\n  namespace: boss\nroleRef:\n  kind: Role\n  name: r\nsubjects:\n  - kind: ServiceAccount\n    name: dev-session\n    namespace: boss-dev\n";
    let block_list = format!(
        "kind: Role\nmetadata:\n  name: r\n  namespace: boss\nrules:\n  - apiGroups: [apps]\n    resources: [deployments]\n    verbs:\n      - patch\n{binding}"
    );
    let found = in_boss(&read(&block_list)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(found.findings.len(), 1, "{:#?}", found.findings);

    let unreadable = block_list.replace("      - patch\n", "      - [patch\n");
    let err = read_stream("fixture.yaml", &unreadable).expect_err("an unclosed list is refused");
    assert!(err.starts_with("fixture.yaml:9:"), "{err}");
}

// ---- every mutation the review ran goes red --------------------------------

const TO_SESSION: &str =
    "  - kind: ServiceAccount\n    name: dev-session\n    namespace: boss-dev\n";

/// A Role `mutant` in `ns` with `rules` (the lines under `rules:`), bound to
/// `subjects` by a RoleBinding in `ns`.
fn granted(ns: &str, rules: &str, subjects: &str) -> String {
    format!(
        "apiVersion: rbac.authorization.k8s.io/v1\nkind: Role\nmetadata:\n  name: mutant\n  namespace: {ns}\nrules:\n{rules}---\napiVersion: rbac.authorization.k8s.io/v1\nkind: RoleBinding\nmetadata:\n  name: mutant\n  namespace: {ns}\nroleRef:\n  apiGroup: rbac.authorization.k8s.io\n  kind: Role\n  name: mutant\nsubjects:\n{subjects}"
    )
}

const PATCH_DEPLOYMENTS: &str =
    "  - apiGroups: [apps]\n    resources: [deployments]\n    verbs: [patch]\n";

#[test]
fn every_review_mutation_goes_red() {
    let e = rbac::estate();
    assert_eq!(
        estate_findings(e).map(|j| j.findings),
        Ok(Vec::new()),
        "the control: the unmutated estate passes"
    );
    let add = |text: &str| e.with_file(&e.source, "m.yaml", text);
    let mutations: Vec<(&str, Estate)> = vec![
        (
            "user-subject",
            add(&granted(
                "boss",
                PATCH_DEPLOYMENTS,
                "  - kind: User\n    name: system:serviceaccount:boss-dev:dev-session\n",
            )),
        ),
        (
            "inline-rules",
            add(&granted("boss", PATCH_DEPLOYMENTS, TO_SESSION).replace(
                &format!("rules:\n{PATCH_DEPLOYMENTS}"),
                "rules: [{apiGroups: [apps], resources: [deployments], verbs: [patch]}]\n",
            )),
        ),
        (
            "inline-metadata-binding",
            add(&granted("boss", PATCH_DEPLOYMENTS, TO_SESSION).replace(
                "metadata:\n  name: mutant\n  namespace: boss\nroleRef",
                "metadata: {name: mutant, namespace: boss}\nroleRef",
            )),
        ),
        (
            "inline-subjects",
            add(&granted("boss", PATCH_DEPLOYMENTS, "").replace(
                "subjects:\n",
                "subjects: [{kind: ServiceAccount, name: dev-session, namespace: boss-dev}]\n",
            )),
        ),
        (
            "single-quoted-ns",
            add(&granted("boss", PATCH_DEPLOYMENTS, TO_SESSION)
                .replace("  namespace: boss\n", "  namespace: 'boss'\n")),
        ),
        (
            "kind-list",
            add(
                "apiVersion: v1\nkind: List\nitems:\n  - apiVersion: rbac.authorization.k8s.io/v1\n    kind: RoleBinding\n    metadata:\n      name: m\n      namespace: boss\n    roleRef:\n      kind: ClusterRole\n      name: cluster-admin\n    subjects:\n      - kind: ServiceAccount\n        name: dev-session\n        namespace: boss-dev\n",
            ),
        ),
        (
            "aggregated-clusterrole",
            add(
                "apiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRole\nmetadata:\n  name: mutant\naggregationRule:\n  clusterRoleSelectors:\n    - matchLabels:\n        rbac.example/aggregate: \"true\"\nrules: []\n---\napiVersion: rbac.authorization.k8s.io/v1\nkind: RoleBinding\nmetadata:\n  name: mutant\n  namespace: boss\nroleRef:\n  kind: ClusterRole\n  name: mutant\nsubjects:\n  - kind: ServiceAccount\n    name: dev-session\n    namespace: boss-dev\n",
            ),
        ),
        (
            "replicationcontrollers",
            add(&granted(
                "boss",
                "  - apiGroups: [\"\"]\n    resources: [replicationcontrollers]\n    verbs: [create, patch]\n",
                TO_SESSION,
            )),
        ),
        (
            "sa-token",
            add(&granted(
                "boss",
                "  - apiGroups: [\"\"]\n    resources: [serviceaccounts/token]\n    verbs: [create]\n",
                TO_SESSION,
            )),
        ),
        (
            "get-secrets",
            add(&granted(
                "boss",
                "  - apiGroups: [\"\"]\n    resources: [secrets]\n    verbs: [get]\n",
                TO_SESSION,
            )),
        ),
        (
            "impersonate-clusterrole-crb",
            add(
                "apiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRole\nmetadata:\n  name: mutant\nrules:\n  - apiGroups: [\"\"]\n    resources: [serviceaccounts]\n    verbs: [impersonate]\n---\napiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRoleBinding\nmetadata:\n  name: mutant\nroleRef:\n  kind: ClusterRole\n  name: mutant\nsubjects:\n  - kind: ServiceAccount\n    name: dev-session\n    namespace: boss-dev\n",
            ),
        ),
        (
            "escalate-bind",
            add(&granted(
                "boss",
                "  - apiGroups: [rbac.authorization.k8s.io]\n    resources: [roles, clusterroles]\n    verbs: [bind, escalate]\n  - apiGroups: [rbac.authorization.k8s.io]\n    resources: [rolebindings]\n    verbs: [create]\n",
                TO_SESSION,
            )),
        ),
        (
            "a grant in the playground namespace",
            e.with_file(
                "boss-playground",
                "m.yaml",
                &granted("boss-playground", PATCH_DEPLOYMENTS, TO_SESSION),
            ),
        ),
        (
            "a tab before a comment on the subject's namespace",
            add(&granted(
                "boss",
                PATCH_DEPLOYMENTS,
                "  - kind: ServiceAccount\n    name: dev-session\n    namespace: boss-dev\t# the session\n",
            )),
        ),
        (
            "a group the session is in",
            add(&granted(
                "boss",
                PATCH_DEPLOYMENTS,
                "  - kind: Group\n    name: system:serviceaccounts:boss-dev\n",
            )),
        ),
        // Review cbb5 (run 42ee3f9b): cluster-scoped credential doors the
        // first deny set missed. Each is granted the only way it takes
        // effect, a ClusterRole bound by a ClusterRoleBinding.
        (
            "get nodes/proxy (a WebSocket exec is authorized as a GET)",
            add(&cluster_granted(
                "  - apiGroups: [\"\"]\n    resources: [nodes/proxy]\n    verbs: [get]\n",
            )),
        ),
        (
            "mint a client certificate (CSR create, approval update, signer approve)",
            add(&cluster_granted(
                "  - apiGroups: [certificates.k8s.io]\n    resources: [certificatesigningrequests]\n    verbs: [create]\n  - apiGroups: [certificates.k8s.io]\n    resources: [certificatesigningrequests/approval]\n    verbs: [update]\n  - apiGroups: [certificates.k8s.io]\n    resources: [signers]\n    resourceNames: [kubernetes.io/kube-apiserver-client]\n    verbs: [approve]\n",
            )),
        ),
        (
            "write a mutating webhook configuration",
            add(&cluster_granted(
                "  - apiGroups: [admissionregistration.k8s.io]\n    resources: [mutatingwebhookconfigurations]\n    verbs: [create]\n",
            )),
        ),
        (
            "write a validating webhook configuration",
            add(&cluster_granted(
                "  - apiGroups: [admissionregistration.k8s.io]\n    resources: [validatingwebhookconfigurations]\n    verbs: [patch]\n",
            )),
        ),
        // The release re-review of cbb5 (backlog cbdf1e9c): kube-controller-
        // manager AUTO-approves a node-client CSR whose requester holds
        // `create` on this subresource, so no approval grant is needed to
        // mint a system:node client certificate.
        (
            "create certificatesigningrequests/nodeclient (auto-approved node cert)",
            add(&cluster_granted(
                "  - apiGroups: [certificates.k8s.io]\n    resources: [certificatesigningrequests/nodeclient]\n    verbs: [create]\n",
            )),
        ),
        (
            "create certificatesigningrequests/selfnodeclient (auto-approved node cert)",
            add(&cluster_granted(
                "  - apiGroups: [certificates.k8s.io]\n    resources: [certificatesigningrequests/selfnodeclient]\n    verbs: [create]\n",
            )),
        ),
        (
            "create on every certificates.k8s.io subresource (*/* covers nodeclient)",
            add(&cluster_granted(
                "  - apiGroups: [certificates.k8s.io]\n    resources: [\"*/*\"]\n    verbs: [create]\n",
            )),
        ),
        // A CEL mutation rewrites a pod's identity or command the way a
        // mutating webhook does; the binding is what makes it apply.
        (
            "write a mutating admission policy",
            add(&cluster_granted(
                "  - apiGroups: [admissionregistration.k8s.io]\n    resources: [mutatingadmissionpolicies]\n    verbs: [create]\n",
            )),
        ),
        (
            "write a mutating admission policy binding",
            add(&cluster_granted(
                "  - apiGroups: [admissionregistration.k8s.io]\n    resources: [mutatingadmissionpolicybindings]\n    verbs: [update]\n",
            )),
        ),
    ];
    let green: Vec<&str> = mutations
        .iter()
        .filter(|(name, m)| {
            let verdict = estate_findings(m).map(|j| j.findings);
            // Each verdict's reason, for the reviewer who runs --nocapture:
            // red for the RIGHT reason is the claim, not red at all.
            eprintln!("MUTATION {name}: {verdict:?}");
            matches!(verdict, Ok(f) if f.is_empty())
        })
        .map(|(name, _)| *name)
        .collect();
    assert!(
        green.is_empty(),
        "these mutations pass the pin — each is a real grant it must refuse: {green:?}"
    );
    // The other control: a read-only grant to the session stays green, so
    // the reds above are the mutations, not the harness.
    let harmless = add(&granted(
        "boss",
        "  - apiGroups: [apps]\n    resources: [deployments, replicasets]\n    verbs: [get, list, watch]\n",
        TO_SESSION,
    ));
    assert_eq!(
        estate_findings(&harmless).map(|j| j.findings),
        Ok(Vec::new())
    );
    // And the neighbours of review cbb5's doors that open none: reading
    // nodes (not their proxy), filing a CSR nobody may approve, reading
    // the webhook configurations.
    let cluster_harmless = add(&cluster_granted(
        "  - apiGroups: [\"\"]\n    resources: [nodes, nodes/status]\n    verbs: [get, list, watch]\n  - apiGroups: [certificates.k8s.io]\n    resources: [certificatesigningrequests]\n    verbs: [create, get]\n  - apiGroups: [admissionregistration.k8s.io]\n    resources: [mutatingwebhookconfigurations, validatingwebhookconfigurations]\n    verbs: [get, list]\n",
    ));
    assert_eq!(
        estate_findings(&cluster_harmless).map(|j| j.findings),
        Ok(Vec::new())
    );
    // The control for backlog cbdf1e9c: `create` on the CSR object ALONE
    // files a request someone else must approve, so it stays green — the
    // node-client reds above are the subresource, not CSR create itself.
    let csr_create_alone = add(&cluster_granted(
        "  - apiGroups: [certificates.k8s.io]\n    resources: [certificatesigningrequests]\n    verbs: [create]\n",
    ));
    assert_eq!(
        estate_findings(&csr_create_alone).map(|j| j.findings),
        Ok(Vec::new())
    );
}

/// A ClusterRole `mutant` with `rules`, bound to the session cluster-wide.
fn cluster_granted(rules: &str) -> String {
    format!(
        "apiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRole\nmetadata:\n  name: mutant\nrules:\n{rules}---\napiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRoleBinding\nmetadata:\n  name: mutant\nroleRef:\n  apiGroup: rbac.authorization.k8s.io\n  kind: ClusterRole\n  name: mutant\nsubjects:\n{TO_SESSION}"
    )
}
