//! The break-glass operator's `patch` is judged by an admission policy
//! that allows exactly one change: a container's or initContainer's
//! image, moved to another build IN THE SAME REPOSITORY, on the three
//! workloads a rollback moves. That policy REFUSES everything else
//! (backlog 4e1c33b4, which bound it to report; design b08725c2 row B,
//! decided by David 2026-09-29 23:33Z; its Deny car is backlog
//! e4a9a9b3).
//!
//! WHY. RBAC scopes `patch` to an object, never to a field, so the
//! `break-glass-operator-rollback` Role (boss-break-glass-operator.yaml)
//! can rewrite the boss pod TEMPLATE — its command, its env, its
//! serviceAccountName, its volumes, its IMAGE — and the controller then
//! starts a pod that hands back every credential the template can name,
//! one `pods/log` read later. The image is on that list: a foreign image's
//! own entrypoint runs with every env secretKeyRef deploy/boss declares
//! (review 89c716e0, B1), so "image only" without "which repository" is
//! no narrowing at all. The intended use is the estate's roll to a NAMED
//! last-converged build — one JSON patch replacing the image of
//! containers/0 AND initContainers/0 (`_patch_boss_image` in
//! infra/forge/cluster-deploy-lib.sh, the same patch in
//! cluster-watchdog.sh) — and a ValidatingAdmissionPolicy is the only
//! place the API server can hold a patch to that.
//!
//! WHAT THIS HOLDS, by EVALUATING the manifest's own CEL — a grep of the
//! expression text would hold its spelling, not its decision:
//!   * the estate's two-image roll, replayed from the two scripts that
//!     send it, passes; so does a same-repository image on each target, by
//!     tag, by digest, and by tag and digest;
//!   * a foreign image on each target is flagged, and so is every
//!     lookalike: the repository as a prefix of a longer name (`boss-ci`,
//!     the CI image, lives beside `boss`), the same path on another
//!     registry host, no tag at all, a tag carrying a `/`, a malformed
//!     digest;
//!   * a command, an env var naming a secretKeyRef, an envFrom, a
//!     serviceAccountName, a volume, a sidecar, a new initContainer, a
//!     template annotation, a replica count, an ownerReference (the
//!     garbage collector deletes an orphan) or a finalizer (which blocks
//!     a deletion, and whose removal lets a pending one finish), each
//!     under its own message, or ANY patch on another workload is flagged;
//!   * the repository each target is held to is the one boss.yaml's own
//!     images name, and deploy/boss's is the one the estate's roll
//!     derives (forge-defaults.sh `REGISTRY`, from estate.toml) — one
//!     fact in three files, pinned equal (CLAUDE.md §9a);
//!   * the policy matches only the break-glass operator;
//!   * the binding's validationActions are exactly Deny and Audit — the
//!     refusing car of b08725c2's order, one refusal per train.
//!
//! WHAT IT CANNOT HOLD, stated: the evaluator is cel-rust, not the API
//! server's cel-go. The expressions use the subset both implement (map
//! key iteration, `in`, index, `==` on maps and lists, `all`,
//! `exists_one`, `size`, `startsWith`, `matches`, `string(int)`, the
//! conditional operator), and the objects are the manifests' own trees
//! with every scalar a string — the same on both sides of every compare,
//! as the API server's unstructured copies are. That the API server
//! ACCEPTS the policy is proven only by the converge applying it
//! (check-manifests-applied.sh then finds it present).
//!
//! tree-wide pin — it reads infra/cluster/manifests/ and infra/forge/,
//! which no changed-file map attributes to this crate, so every scoped
//! gate runs it whatever its scope (`tree_wide_pins` in infra/gate.sh).

use boss_testing::rbac::{Node, Object, Value as Yaml, read_stream};
use boss_testing::{repo_root, scratch_dir};
use cel::{Context, Program, Value as Cel};
use serde_json::{Value as Json, json};
use std::collections::{BTreeSet, HashMap};
use std::process::Command;

const MANIFEST: &str = "infra/cluster/manifests/boss-break-glass-operator.yaml";
const WORKLOADS: &str = "infra/cluster/manifests/boss.yaml";
const CLOUDFLARED: &str = "infra/cluster/manifests/cloudflared.yaml";
const ROLL_LIB: &str = "infra/forge/cluster-deploy-lib.sh";
const WATCHDOG: &str = "infra/forge/cluster-watchdog.sh";
const POLICY: &str = "break-glass-operator-image-only";
const OPERATOR: &str = "system:serviceaccount:boss:break-glass-operator";
/// Any other identity: the converge applies as the forge's admin.
const ADMIN: &str = "admin";
/// A well-formed sha256 digest, for the digest forms.
const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn read(path: &str) -> (String, Vec<Object>) {
    let text =
        std::fs::read_to_string(repo_root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    let objects = read_stream(path, &text).unwrap_or_else(|e| panic!("{e}"));
    (text, objects)
}

fn one<'a>(objects: &'a [Object], kind: &str, name: &str) -> &'a Object {
    objects
        .iter()
        .find(|o| o.kind == kind && o.name == name)
        .unwrap_or_else(|| panic!("no {kind}/{name} is declared"))
}

/// The text of `key:` at `node`: a scalar written inline, or a block
/// scalar (`|`) read from the file, because the shared reader skips a
/// block scalar's text by design.
fn text_of(file: &str, key: &str, node: &Node) -> String {
    match &node.value {
        Yaml::Str(s) => s.clone(),
        Yaml::Text => {
            let lines: Vec<&str> = file.lines().collect();
            let header = lines[node.line - 1];
            let column = header
                .find(&format!("{key}:"))
                .unwrap_or_else(|| panic!("line {} carries no `{key}:`", node.line));
            let body: Vec<&str> = lines[node.line..]
                .iter()
                .take_while(|l| l.trim().is_empty() || l.len() - l.trim_start().len() > column)
                .copied()
                .collect();
            body.iter()
                .map(|l| l.trim())
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string()
        }
        other => panic!("`{key}:` at line {} is not text: {other:?}", node.line),
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
        .map(|n| {
            n.str()
                .unwrap_or_else(|| panic!("{keys:?}: {n:?}"))
                .to_string()
        })
        .collect()
}

struct Policy {
    failure_policy: String,
    conditions: Vec<String>,
    /// (name, expression), in declaration order — a variable may read
    /// the ones above it, as on the API server.
    variables: Vec<(String, String)>,
    /// (expression, message)
    validations: Vec<(String, String)>,
}

fn policy() -> (Policy, Object) {
    let (text, objects) = read(MANIFEST);
    let vap = one(&objects, "ValidatingAdmissionPolicy", POLICY);
    let spec = vap.root.get("spec").expect("the policy has a spec");
    let expression = |n: &Node| {
        text_of(
            &text,
            "expression",
            n.get("expression").expect("expression"),
        )
    };
    let conditions = items(spec, &["matchConditions"])
        .iter()
        .map(expression)
        .collect();
    let variables = items(spec, &["variables"])
        .iter()
        .map(|v| (str_at(v, &["name"]).to_string(), expression(v)))
        .collect();
    let validations = items(spec, &["validations"])
        .iter()
        .map(|v| (expression(v), str_at(v, &["message"]).to_string()))
        .collect();
    let binding = objects
        .iter()
        .find(|o| {
            o.kind == "ValidatingAdmissionPolicyBinding"
                && o.root.path(&["spec", "policyName"]).and_then(Node::str) == Some(POLICY)
        })
        .unwrap_or_else(|| panic!("no ValidatingAdmissionPolicyBinding binds {POLICY}"))
        .clone();
    (
        Policy {
            failure_policy: str_at(spec, &["failurePolicy"]).to_string(),
            conditions,
            variables,
            validations,
        },
        binding,
    )
}

fn to_json(node: &Node) -> Json {
    match &node.value {
        Yaml::Map(entries) => Json::Object(
            entries
                .iter()
                .map(|(k, n)| (k.clone(), to_json(n)))
                .collect(),
        ),
        Yaml::Seq(items) => Json::Array(items.iter().map(to_json).collect()),
        Yaml::Str(s) => Json::String(s.clone()),
        Yaml::Text => Json::String("<block scalar>".into()),
        Yaml::Null => Json::Null,
    }
}

/// A live workload as the manifests declare it.
fn workload(path: &str, kind: &str, name: &str) -> Json {
    let (_, objects) = read(path);
    to_json(&one(&objects, kind, name).root)
}

/// Marks a validation that failed by ERRORING rather than answering
/// false. The API server treats both alike under failurePolicy Fail, but
/// a flag this pin reached only through an error is a place cel-rust and
/// cel-go may part ways, so a flagged mutation must be flagged by `false`.
const ERRORED: &str = "[ERRORED]";

fn eval(expr: &str, ctx: &Context) -> Result<Cel, String> {
    let program = Program::compile(expr).map_err(|e| format!("does not compile: {e}"))?;
    program.execute(ctx).map_err(|e| format!("errors: {e}"))
}

/// `None` when the policy does not match the request; otherwise the
/// message of every validation it fails. An expression that ERRORS is a
/// failure, as failurePolicy Fail makes it on the API server.
fn judge(p: &Policy, kind: &str, user: &str, old: &Json, new: &Json) -> Option<Vec<String>> {
    let request = json!({
        "kind": {"group": "apps", "version": "v1", "kind": kind},
        "operation": "UPDATE",
        "namespace": "boss",
        "name": new["metadata"]["name"],
        "userInfo": {"username": user},
    });
    let mut ctx = Context::default();
    ctx.add_variable("request", &request).expect("request");
    ctx.add_variable("object", new).expect("object");
    ctx.add_variable("oldObject", old).expect("oldObject");
    for condition in &p.conditions {
        match eval(condition, &ctx) {
            Ok(Cel::Bool(true)) => {}
            Ok(Cel::Bool(false)) => return None,
            other => panic!("matchCondition {condition:?} answered {other:?}"),
        }
    }
    // Variables are evaluated lazily on the API server; eagerly here,
    // and one that errors is a failure of every validation reading it.
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

/// Flagged, and by a validation answering false — never only by an error.
fn flagged_by_false(verdict: &Option<Vec<String>>) -> bool {
    verdict
        .as_ref()
        .is_some_and(|f| !f.is_empty() && !f.iter().any(|m| m.contains(ERRORED)))
}

fn container<'a>(obj: &'a mut Json, list: &str, name: &str) -> &'a mut Json {
    obj["spec"]["template"]["spec"][list]
        .as_array_mut()
        .unwrap_or_else(|| panic!("no {list}"))
        .iter_mut()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no container {name}"))
}

fn changed(old: &Json, edit: impl FnOnce(&mut Json)) -> Json {
    let mut new = old.clone();
    edit(&mut new);
    assert_ne!(&new, old, "a mutation that changes nothing tests nothing");
    new
}

/// Every image a workload's pod template names, containers and
/// initContainers alike, as (list, container name, image).
fn images(obj: &Json) -> Vec<(String, String, String)> {
    ["containers", "initContainers"]
        .iter()
        .flat_map(|list| {
            obj["spec"]["template"]["spec"][*list]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(move |c| {
                    (
                        list.to_string(),
                        c["name"].as_str().unwrap_or_default().to_string(),
                        c["image"].as_str().unwrap_or_default().to_string(),
                    )
                })
        })
        .collect()
}

/// An image reference's repository: the reference without its digest,
/// and without its tag — a `:` after the last `/` (a `:` before it is
/// the registry's port).
fn repository_of(image: &str) -> String {
    let name = image.split('@').next().unwrap_or(image);
    match (name.rfind(':'), name.rfind('/')) {
        (Some(colon), slash) if slash.is_none_or(|s| colon > s) => name[..colon].to_string(),
        _ => name.to_string(),
    }
}

/// Every image of the workload moved to `image`, as a set-image across
/// the template would — the images of one target share one repository.
fn with_images(old: &Json, image: &str) -> Json {
    let mut new = old.clone();
    for (list, name, _) in images(old) {
        container(&mut new, &list, &name)["image"] = json!(image);
    }
    new
}

/// The three rollback targets, each as the manifest declares it, with the
/// one repository every image in its template is pulled from.
fn targets() -> Vec<(&'static str, &'static str, Json, String)> {
    [
        ("Deployment", "boss"),
        ("StatefulSet", "postgres"),
        ("StatefulSet", "nats"),
    ]
    .into_iter()
    .map(|(kind, name)| {
        let obj = workload(WORKLOADS, kind, name);
        let repos: BTreeSet<String> = images(&obj)
            .iter()
            .map(|(_, _, image)| repository_of(image))
            .collect();
        assert_eq!(
            repos.len(),
            1,
            "{kind}/{name} in {WORKLOADS} pulls from {repos:?}; the policy holds a target to \
             ONE repository"
        );
        let repo = repos.into_iter().next().unwrap_or_default();
        (kind, name, obj, repo)
    })
    .collect()
}

/// The repository the estate's roll moves deploy/boss to, derived the way
/// the watchdog and rollback-to derive it: /etc/boss/sor.env rendered from
/// infra/estate/estate.toml, then forge-defaults.sh's `forge_need
/// REGISTRY`, in an environment that names no override.
fn roll_registry() -> String {
    let root = repo_root();
    let dir = scratch_dir("break-glass-roll-registry");
    let env_file = dir.join("sor.env");
    let rendered = Command::new("bash")
        .arg(root.join("infra/estate/render-sor-env.sh"))
        .output()
        .expect("render-sor-env.sh runs");
    assert!(
        rendered.status.success(),
        "render-sor-env.sh: {}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    std::fs::write(&env_file, &rendered.stdout).expect("write sor.env");
    let out = Command::new("bash")
        .arg("-c")
        .arg("set -u; . infra/forge/forge-defaults.sh && forge_need REGISTRY && printf '%s' \"$REGISTRY\"")
        .current_dir(&root)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_SOR_ENV", &env_file)
        .output()
        .expect("forge-defaults.sh runs");
    assert!(
        out.status.success(),
        "forge_need REGISTRY: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

/// The JSON patch a script sends with `kubectl patch deploy boss
/// --type=json -p "…"`, read from the script, with `var` (the shell word
/// that carries the image) replaced by `image`.
fn roll_patch(script: &str, after: &str, var: &str, image: &str) -> Json {
    let text = std::fs::read_to_string(repo_root().join(script)).expect("script");
    let from = text
        .find(after)
        .unwrap_or_else(|| panic!("{script} carries no `{after}`"));
    let line = text[from..]
        .lines()
        .find(|l| l.contains("-p \"[{"))
        .unwrap_or_else(|| panic!("{script}: no `-p \"[{{` after `{after}`"));
    let start = line.find("-p \"").expect("-p") + 4;
    let end = line.rfind("]\"").expect("closing ]\"") + 1;
    let body = line[start..end].replace("\\\"", "\"").replace(var, image);
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{script}: {e}: {body}"))
}

/// A JSON patch of `replace` and `add` ops, applied as the API server
/// would: `replace` needs the member to exist, `add` sets it on an
/// existing parent object whether or not it exists yet.
fn apply(old: &Json, patch: &Json) -> Json {
    let mut new = old.clone();
    for op in patch.as_array().expect("a JSON patch is a list") {
        let path = op["path"].as_str().expect("path");
        match op["op"].as_str() {
            Some("replace") => {
                *new.pointer_mut(path)
                    .unwrap_or_else(|| panic!("{path} is not in the object")) = op["value"].clone();
            }
            Some("add") => {
                let (parent, key) = path.rsplit_once('/').expect("a path with a parent");
                new.pointer_mut(parent)
                    .and_then(Json::as_object_mut)
                    .unwrap_or_else(|| panic!("{parent} is not an object in the object"))
                    .insert(key.to_string(), op["value"].clone());
            }
            other => panic!("an op this replay does not apply: {other:?} in {op}"),
        }
    }
    new
}

/// The break-glass deposit (infra/cluster/break-glass-deposit.sh, item
/// 7336cb5f) verifies the credential it ships by sending, as that
/// credential, server-side dry-run patches it `must_refuse`. Each is a
/// write this policy exists for, so each must be FLAGGED by it — by a
/// validation answering false — whatever the binding's actions are.
/// Read from the script, so a probe added there is judged here too.
#[test]
fn the_deposits_must_refuse_patches_are_each_flagged() {
    const DEPOSIT: &str = "infra/cluster/break-glass-deposit.sh";
    let (p, _) = policy();
    let text = std::fs::read_to_string(repo_root().join(DEPOSIT)).expect("deposit script");
    let calls = text
        .lines()
        .filter(|l| l.starts_with("must_refuse \""))
        .count();
    let patches: Vec<Json> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("'[{\"op\":") && l.ends_with("]'"))
        .map(|l| {
            serde_json::from_str(&l[1..l.len() - 1])
                .unwrap_or_else(|e| panic!("{DEPOSIT}: {e}: {l}"))
        })
        .collect();
    assert!(
        calls > 0 && patches.len() == calls,
        "{DEPOSIT} makes {calls} must_refuse call(s) but {} patch line(s) were read — the \
         replay would judge a different set than the deposit sends",
        patches.len()
    );
    let old = workload(WORKLOADS, "Deployment", "boss");
    for patch in &patches {
        let new = apply(&old, patch);
        assert_ne!(new, old, "{patch} changed nothing");
        let verdict = judge(&p, "Deployment", OPERATOR, &old, &new);
        assert!(
            flagged_by_false(&verdict),
            "{DEPOSIT} must_refuse {patch} on deploy/boss, but {POLICY} does not flag it: \
             {verdict:?}"
        );
    }
}

#[test]
fn a_same_repository_image_on_each_rollback_target_passes() {
    let (p, _) = policy();
    assert!(
        !p.validations.is_empty(),
        "a policy with no validations judges nothing"
    );
    for (kind, name, old, repo) in targets() {
        for image in [
            format!("{repo}:rollback-target"),
            format!("{repo}@{DIGEST}"),
            format!("{repo}:rollback-target@{DIGEST}"),
        ] {
            let new = changed(&old, |o| *o = with_images(o, &image));
            assert_eq!(
                judge(&p, kind, OPERATOR, &old, &new),
                Some(vec![]),
                "moving {kind}/{name} to {image} — a named build in its own repository — is \
                 the road this credential exists for, and must pass"
            );
        }
    }
}

#[test]
fn the_estates_two_image_roll_to_a_named_build_passes() {
    let (p, _) = policy();
    let old = workload(WORKLOADS, "Deployment", "boss");
    let image = format!("{}:abc1234", roll_registry());
    for (script, after, var) in [
        (ROLL_LIB, "_patch_boss_image() {", "$image"),
        (
            WATCHDOG,
            "rolling deploy/boss to the last",
            "$REGISTRY:$stamp",
        ),
    ] {
        let patch = roll_patch(script, after, var, &image);
        let paths: Vec<&str> = patch
            .as_array()
            .expect("list")
            .iter()
            .filter_map(|op| op["path"].as_str())
            .collect();
        assert!(
            paths.contains(&"/spec/template/spec/containers/0/image")
                && paths.contains(&"/spec/template/spec/initContainers/0/image"),
            "{script}'s roll is expected to move BOTH images in one patch (a main-only roll \
             mints a mixed template, cluster-deploy-runner.sh); it moves {paths:?}"
        );
        let new = apply(&old, &patch);
        assert_ne!(new, old, "{script}: the replayed roll changed nothing");
        assert_eq!(
            judge(&p, "Deployment", OPERATOR, &old, &new),
            Some(vec![]),
            "{script}'s roll to a named build, sent as the break-glass operator, must pass — \
             it is the road this credential exists for"
        );
    }
}

#[test]
fn a_foreign_or_lookalike_image_on_any_target_is_flagged() {
    let (p, _) = policy();
    for (kind, name, old, repo) in targets() {
        let path = repo.split_once('/').map(|(_, path)| path).unwrap_or(&repo);
        let mut candidates = vec![
            ("a foreign image", "docker.io/anyone/printenv:1".to_string()),
            (
                "a longer name the repository prefixes",
                format!("{repo}-ci:1"),
            ),
            (
                "a longer name glued to the repository",
                format!("{repo}evil:1"),
            ),
            (
                "the same path on another registry host",
                format!("ghcr.io/{path}:1"),
            ),
            (
                "the same path on Docker Hub's canonical host",
                format!("docker.io/library/{path}:1"),
            ),
            ("the repository with no tag", repo.clone()),
            ("an empty tag", format!("{repo}:")),
            (
                "a tag carrying a path",
                format!("{repo}:1/../../anyone/printenv"),
            ),
            ("a malformed digest", format!("{repo}@sha256:abc")),
            (
                "a repository under the target's",
                format!("{repo}/printenv:1"),
            ),
        ];
        if repo.contains('/') {
            candidates.push((
                "a registry prefixed to the path",
                format!("evil.example/{repo}:1"),
            ));
        }
        for (what, image) in candidates {
            // Every image moved, and — separately — only the first one,
            // so a list judged by `exists` rather than `all` is caught.
            let whole = with_images(&old, &image);
            let (list, cname, _) = images(&old).remove(0);
            let first = changed(&old, |o| {
                container(o, &list, &cname)["image"] = json!(image)
            });
            for new in [whole, first] {
                let verdict = judge(&p, kind, OPERATOR, &old, &new);
                assert!(
                    flagged_by_false(&verdict),
                    "{kind}/{name} moved to {image} ({what}) must be flagged by a validation \
                     answering false — an image outside {repo} runs its own entrypoint with \
                     every credential the template names (review 89c716e0 B1); got {verdict:?}"
                );
            }
        }
    }
    // The init image alone, on deploy/boss: boss-init runs with the same
    // Secrets, so it is held to the same repository.
    let old = workload(WORKLOADS, "Deployment", "boss");
    let new = changed(&old, |o| {
        container(o, "initContainers", "boss-init")["image"] = json!("docker.io/anyone/printenv:1");
    });
    let verdict = judge(&p, "Deployment", OPERATOR, &old, &new);
    assert!(
        flagged_by_false(&verdict),
        "a foreign boss-init image must be flagged: {verdict:?}"
    );
}

#[test]
fn a_template_or_metadata_write_that_reads_a_credential_or_deletes_is_flagged() {
    let (p, _) = policy();
    let old = workload(WORKLOADS, "Deployment", "boss");
    let secret = json!({"secretKeyRef": {"name": "boss-credential-broker-root", "key": "x"}});
    let mutations: Vec<(&str, Json)> = vec![
        (
            "the command",
            changed(&old, |o| {
                container(o, "containers", "boss")["command"] = json!(["sh", "-c", "env"]);
            }),
        ),
        (
            "the args",
            changed(&old, |o| {
                container(o, "containers", "boss")["args"] = json!(["--print-env"]);
            }),
        ),
        (
            "an env var naming a Secret",
            changed(&old, |o| {
                container(o, "containers", "boss")["env"]
                    .as_array_mut()
                    .expect("env")
                    .push(json!({"name": "LEAK", "valueFrom": secret}));
            }),
        ),
        (
            "an envFrom",
            changed(&old, |o| {
                container(o, "containers", "boss")["envFrom"] =
                    json!([{"secretRef": {"name": "boss-secrets"}}]);
            }),
        ),
        (
            "the init container's command",
            changed(&old, |o| {
                container(o, "initContainers", "boss-init")["command"] = json!(["env"]);
            }),
        ),
        (
            "the image AND the command together",
            changed(&old, |o| {
                let c = container(o, "containers", "boss");
                c["image"] = json!("attacker/image:latest");
                c["command"] =
                    json!(["cat", "/var/run/secrets/kubernetes.io/serviceaccount/token"]);
            }),
        ),
        (
            "the serviceAccountName",
            changed(&old, |o| {
                o["spec"]["template"]["spec"]["serviceAccountName"] = json!("credential-broker");
            }),
        ),
        (
            "a volume",
            changed(&old, |o| {
                o["spec"]["template"]["spec"]["volumes"]
                    .as_array_mut()
                    .expect("volumes")
                    .push(json!({"name": "leak", "secret": {"secretName": "boss-secrets"}}));
            }),
        ),
        (
            "a sidecar container",
            changed(&old, |o| {
                o["spec"]["template"]["spec"]["containers"]
                .as_array_mut()
                .expect("containers")
                .push(json!({"name": "leak", "image": "busybox", "env": [{"name": "L", "valueFrom": secret}]}));
            }),
        ),
        (
            "a second initContainer",
            changed(&old, |o| {
                o["spec"]["template"]["spec"]["initContainers"]
                    .as_array_mut()
                    .expect("initContainers")
                    .push(json!({"name": "leak", "image": "busybox", "command": ["env"]}));
            }),
        ),
        (
            "the initContainers removed",
            changed(&old, |o| {
                o["spec"]["template"]["spec"]
                    .as_object_mut()
                    .expect("pod spec")
                    .remove("initContainers");
            }),
        ),
        (
            "a template annotation (rollout restart)",
            changed(&old, |o| {
                o["spec"]["template"]["metadata"]["annotations"] =
                    json!({"kubectl.kubernetes.io/restartedAt": "2026-09-30T00:00:00Z"});
            }),
        ),
        (
            "the replica count",
            changed(&old, |o| {
                o["spec"]["replicas"] = json!("0");
            }),
        ),
        (
            "a container renamed",
            changed(&old, |o| {
                container(o, "containers", "boss")["name"] = json!("boss2");
            }),
        ),
        (
            "an ownerReference (the garbage collector deletes an orphan)",
            changed(&old, |o| {
                o["metadata"]["ownerReferences"] = json!([{
                    "apiVersion": "v1", "kind": "ConfigMap", "name": "gone",
                    "uid": "00000000-0000-0000-0000-000000000000"
                }]);
            }),
        ),
        (
            "a finalizer",
            changed(&old, |o| {
                o["metadata"]["finalizers"] = json!(["example.com/hold"]);
            }),
        ),
    ];
    let verdicts: Vec<(&str, Option<Vec<String>>)> = mutations
        .iter()
        .map(|(what, new)| (*what, judge(&p, "Deployment", OPERATOR, &old, new)))
        .collect();
    let missed: Vec<&(&str, Option<Vec<String>>)> = verdicts
        .iter()
        .filter(|(_, verdict)| !flagged_by_false(verdict))
        .collect();
    assert!(
        missed.is_empty(),
        "the break-glass policy did not flag, by a validation answering false, a patch of \
         deploy/boss that changes: {missed:?} — each is a write that can hand back a credential \
         or remove the workload (backlog 4e1c33b4)"
    );
}

#[test]
fn a_patch_on_any_other_workload_is_flagged() {
    let (p, _) = policy();
    let cloudflared = workload(CLOUDFLARED, "Deployment", "cloudflared");
    let own = images(&cloudflared).remove(0).2;
    let image = format!("{}:2099.1.1", repository_of(&own));
    let new = changed(&cloudflared, |o| *o = with_images(o, &image));
    let verdict = judge(&p, "Deployment", OPERATOR, &cloudflared, &new);
    assert!(
        flagged_by_false(&verdict),
        "an image patch on deploy/cloudflared (which holds the tunnel's credentials), even \
         within its own repository, must be flagged by a validation answering false: only \
         boss, postgres and nats are rollback targets — got {verdict:?}"
    );
    // A StatefulSet named boss is not a rollback target either: the name
    // is judged WITH its kind.
    let (_, _, boss, repo) = targets().remove(0);
    let new = changed(&boss, |o| *o = with_images(o, &format!("{repo}:x")));
    let verdict = judge(&p, "StatefulSet", OPERATOR, &boss, &new);
    assert!(
        flagged_by_false(&verdict),
        "the name must be judged with its kind: {verdict:?}"
    );
}

#[test]
fn each_targets_repository_is_the_one_its_manifest_and_its_roll_name() {
    let (p, _) = policy();
    let (name, expr) = p
        .variables
        .iter()
        .find(|(name, _)| name == "repositories")
        .unwrap_or_else(|| panic!("{POLICY} declares no `repositories` variable"));
    let ctx = Context::default();
    assert_eq!(
        eval(&format!("size({expr})"), &ctx),
        Ok(Cel::Int(3)),
        "variables.{name} must name exactly the three rollback targets"
    );
    let targets = targets();
    for (kind, target, _, repo) in &targets {
        let declared = eval(&format!("({expr})['{kind}/{target}']"), &ctx);
        assert_eq!(
            declared,
            Ok(Cel::String(repo.clone().into())),
            "{POLICY} holds {kind}/{target} to {declared:?}, but {WORKLOADS} pulls it from \
             {repo} — one fact in two files (CLAUDE.md §9a): change both"
        );
    }
    let roll = roll_registry();
    assert_eq!(
        targets[0].3, roll,
        "deploy/boss's repository in {WORKLOADS} ({}) is not the one the estate's roll \
         derives from estate.toml ({roll}): the watchdog would roll to an image the policy \
         flags",
        targets[0].3
    );
}

#[test]
fn it_judges_only_the_break_glass_operator() {
    let (p, _) = policy();
    let old = workload(WORKLOADS, "Deployment", "boss");
    let new = changed(&old, |o| {
        container(o, "containers", "boss")["command"] = json!(["boss", "--new-flag"]);
    });
    assert_eq!(
        judge(&p, "Deployment", ADMIN, &old, &new),
        None,
        "the converge writes every field of the template on every train; the policy must \
         match only {OPERATOR}"
    );
    assert!(
        judge(&p, "Deployment", OPERATOR, &old, &new).is_some(),
        "control: the same patch from the break-glass operator is judged"
    );
}

#[test]
fn the_policy_matches_updates_to_both_kinds_in_boss_and_fails_closed() {
    let (p, _) = policy();
    let (_, objects) = read(MANIFEST);
    let spec = one(&objects, "ValidatingAdmissionPolicy", POLICY)
        .root
        .get("spec")
        .expect("spec");
    assert_eq!(
        p.failure_policy, "Fail",
        "an expression that errors must refuse under Deny and reach Audit — \
         Ignore would make a malformed object silent"
    );
    assert_eq!(
        str_at(
            spec,
            &[
                "matchConstraints",
                "namespaceSelector",
                "matchLabels",
                "kubernetes.io/metadata.name"
            ]
        ),
        "boss"
    );
    let rules = items(spec, &["matchConstraints", "resourceRules"]);
    let covers = |resource: &str| {
        rules.iter().any(|r| {
            strings(r, &["apiGroups"]).iter().any(|g| g == "apps")
                && strings(r, &["resources"]).iter().any(|x| x == resource)
                && strings(r, &["operations"])
                    .iter()
                    .any(|o| o == "UPDATE" || o == "*")
                && r.get("resourceNames").is_none()
        })
    };
    assert!(
        covers("deployments") && covers("statefulsets"),
        "UPDATE on deployments and statefulsets must be matched with no resourceNames — \
         narrowing the MATCH to boss/postgres/nats would leave a patch on any other \
         workload unjudged, rather than flagged"
    );
}

/// Review 89c716e0/87fc0e0c n1 (backlog e4a9a9b3): the one validation that
/// judged both fields said a FINALIZER lets the garbage collector delete
/// the workload. A finalizer does the opposite — it holds a deletion open,
/// and removing one lets a pending deletion finish. A holder reading that
/// refusal, or an operator reading the audit log, must be told what the
/// patch actually does, so each field has its own validation and its own
/// message.
#[test]
fn an_owner_reference_and_a_finalizer_are_each_flagged_for_what_they_do() {
    let (p, _) = policy();
    let old = workload(WORKLOADS, "Deployment", "boss");
    let owner = changed(&old, |o| {
        o["metadata"]["ownerReferences"] = json!([{
            "apiVersion": "v1", "kind": "ConfigMap", "name": "gone",
            "uid": "00000000-0000-0000-0000-000000000000"
        }]);
    });
    let finalizer = changed(&old, |o| {
        o["metadata"]["finalizers"] = json!(["example.com/hold"]);
    });
    let said = |new: &Json| judge(&p, "Deployment", OPERATOR, &old, new).unwrap_or_default();
    let owner_said = said(&owner);
    let finalizer_said = said(&finalizer);
    assert!(
        owner_said.len() == 1 && owner_said[0].contains("garbage collector"),
        "an ownerReference is flagged once, naming the garbage collector that deletes: {owner_said:?}"
    );
    assert!(
        finalizer_said.len() == 1
            && finalizer_said[0].contains("finalizer")
            && finalizer_said[0].contains("blocks")
            && !finalizer_said[0].contains("garbage collector"),
        "a finalizer is flagged once, for BLOCKING a deletion — it deletes nothing: {finalizer_said:?}"
    );
}

/// Row B of design b08725c2, its refusing car (backlog e4a9a9b3): the
/// report phase ran as [Warn, Audit] from c073ba30, and the deposit's
/// dry runs of 2026-09-30 17:20Z (packet 9cb136a0) read all three
/// must-refuse patches flagged by this policy and nothing else from the
/// break-glass account. Deny refuses them; Audit keeps each refusal in
/// the API server's audit log. Warn is dropped: under Deny the holder's
/// kubectl already prints the refusal as an error.
#[test]
fn the_binding_denies_and_audits() {
    let (_, binding) = policy();
    let actions = strings(&binding.root, &["spec", "validationActions"]);
    assert_eq!(
        actions,
        ["Deny", "Audit"],
        "{}: the narrowing refuses (Deny) and the refusal is kept (Audit). Moving it OFF \
         Deny is the misfire rollback — deleting the binding over the LAN kube road — \
         or a revert car, never a quiet edit here",
        binding.label()
    );
    assert!(
        binding.root.path(&["spec", "matchResources"]).is_none(),
        "a binding-level matchResources could narrow what the policy judges out of sight \
         of its own matchConstraints"
    );
}
