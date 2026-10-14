//! tree-wide pin — it reads every instance's render of the cluster
//! manifests and the dispatcher's source, which no changed-file map
//! attributes to this crate, so every scoped gate runs it.
//!
//! The broker-stage door turns on WHOLE, in every instance, or not at all
//! (design 6e28ed42; activation, backlog 25fff647).
//!
//! WHAT IS HELD. The door (`boss_jobs::credentials::broker_stage`) is
//! configured by five `BOSS_BROKER_STAGE_*` variables, and its one caller,
//! the dispatcher's runner-credential handler, presents the token the file
//! named by `BOSS_BROKER_STAGE_TOKEN_FILE` holds. Those six values and the
//! projected volume that fills the file live in one manifest, boss.yaml,
//! and are one fact spelled in three places (CLAUDE.md §9a): the audience
//! the kubelet asks for must be the one the door accepts, the lifetime it
//! asks for must be one the door believes, and the file the dispatcher
//! reads must be the file the kubelet writes. So this file runs the
//! manifest's own values through the door's own constructor, per instance,
//! and refuses every pair that disagrees.
//!
//! WHY PER INSTANCE. boss.yaml renders once per instance namespace
//! (render-instance.sh), and the render substitutes `namespace:` lines,
//! not env values. A literal `boss` namespace would have the playground's
//! door expecting prod's workload and refusing its own; the namespace and
//! ServiceAccount are read through the downward API instead, and this pin
//! resolves them the way the kubelet does, for each instance.
//!
//! WHAT IS NOT HELD HERE. That the API server does not accept the chosen
//! audience (review F2) — no file in the tree says what the API server
//! accepts; the door refuses an audience equal to the issuer, the API
//! server's default audience, and this pin refuses an audience spelled as
//! an https URL. That the key set fetch verifies TLS — the door's own
//! tests (`credentials::broker_stage::tests`) hold that.

use boss_jobs::credentials::broker_stage::{
    AUDIENCE_ENV, Door, ISSUER_ENV, KEY_SET_ENV, MAX_LIFETIME_SECS, NAMESPACE_ENV,
    SERVICE_ACCOUNT_ENV,
};
use boss_testing::rbac::{Estate, Node, Object, Value, estate};
use boss_testing::repo_root;
use std::collections::BTreeMap;

/// Where the dispatcher spells the token-file variable's name — read, not
/// copied, because boss-testing does not depend on the dispatcher crate.
const DISPATCHER_SOURCE: &str =
    "crates/orchestrators/boss-dispatcher-handlers/src/handlers/credential_rotate_ops_runner.rs";
const CONTAINER: &str = "boss";

fn token_file_env() -> String {
    let text = std::fs::read_to_string(repo_root().join(DISPATCHER_SOURCE))
        .expect("the dispatcher's runner handler");
    let line = text
        .lines()
        .find(|l| l.starts_with("pub const STAGE_TOKEN_FILE_ENV: &str = \""))
        .expect("the dispatcher spells STAGE_TOKEN_FILE_ENV");
    line.split('"').nth(1).expect("a quoted name").to_string()
}

fn seq(node: Option<&Node>) -> &[Node] {
    match node.map(|n| &n.value) {
        Some(Value::Seq(items)) => items,
        _ => &[],
    }
}

fn text(node: &Node, key: &str) -> Option<String> {
    node.get(key).and_then(Node::str).map(str::to_string)
}

/// The boss container's environment as the kubelet hands it over: a
/// `value`, or a downward-API `fieldRef` resolved against this object.
fn environment(object: &Object, container: &Node) -> Result<BTreeMap<String, String>, String> {
    let namespace = object
        .namespace
        .clone()
        .ok_or("the Deployment names no namespace")?;
    let account = object.service_account().unwrap_or_default();
    let mut env = BTreeMap::new();
    for entry in seq(container.get("env")) {
        let name = text(entry, "name").ok_or("an env entry without a name")?;
        let value = match (
            text(entry, "value"),
            entry
                .path(&["valueFrom", "fieldRef", "fieldPath"])
                .and_then(Node::str),
        ) {
            (Some(v), None) => v,
            (None, Some("metadata.namespace")) => namespace.clone(),
            (None, Some("spec.serviceAccountName")) => account.clone(),
            (None, Some(other)) if name.starts_with("BOSS_BROKER_STAGE_") => {
                return Err(format!(
                    "{name} reads fieldRef {other}, which this pin cannot resolve"
                ));
            }
            _ => continue,
        };
        env.insert(name, value);
    }
    Ok(env)
}

/// Every reason the instance's boss pod does not turn the door on whole.
fn judge(estate: &Estate, instance: &str) -> Result<String, String> {
    let objects = estate.objects_in(instance)?;
    let deployment = objects
        .iter()
        .find(|o| o.kind == "Deployment" && o.name == "boss")
        .ok_or_else(|| format!("{instance}: no Deployment boss"))?;
    let spec = deployment.pod_spec().ok_or("no pod spec")?;
    let container = seq(spec.get("containers"))
        .iter()
        .find(|c| text(c, "name").as_deref() == Some(CONTAINER))
        .ok_or_else(|| format!("{instance}: no container {CONTAINER}"))?;
    let env = environment(deployment, container)?;
    // Each of the five by name, before the door's own all-or-nothing read:
    // a refusal here names the one that is missing.
    for name in [
        ISSUER_ENV,
        AUDIENCE_ENV,
        NAMESPACE_ENV,
        SERVICE_ACCOUNT_ENV,
        KEY_SET_ENV,
    ] {
        if !env.contains_key(name) {
            return Err(format!(
                "{instance}: the {CONTAINER} container does not set {name}"
            ));
        }
    }

    // The door, built from the manifest's values by the door's own
    // constructor — exactly what `Door::from_env` reads at boot.
    let door = Door::from_vars(|name| env.get(name).cloned());
    let Door::On { terms, keys } = &door else {
        return Err(format!(
            "{instance}: the door is not on: {}",
            door.describe()
        ));
    };
    if terms.namespace != deployment.namespace.clone().unwrap_or_default()
        || Some(terms.service_account.clone()) != deployment.service_account()
    {
        return Err(format!(
            "{instance}: the door expects workload {}/{} and the pod runs as {:?}/{:?}",
            terms.namespace,
            terms.service_account,
            deployment.namespace,
            deployment.service_account()
        ));
    }
    if terms.audience.starts_with("https://") {
        return Err(format!(
            "{instance}: {AUDIENCE_ENV} {} is spelled like an API server audience",
            terms.audience
        ));
    }
    if !keys.source().starts_with("https://") {
        return Err(format!(
            "{instance}: {KEY_SET_ENV} is not fetched from the cluster: {}",
            keys.source()
        ));
    }
    // The key fetch presents the pod's own token: it must be mounted.
    if text(spec, "automountServiceAccountToken").as_deref() != Some("true") {
        return Err(format!(
            "{instance}: the pod does not mount its own ServiceAccount token, which the key fetch presents"
        ));
    }

    // The projection: ONE source, a token for exactly the door's audience,
    // for exactly the door's lifetime.
    let projected: Vec<(&Node, &Node)> = seq(spec.get("volumes"))
        .iter()
        .flat_map(|v| {
            seq(v.path(&["projected", "sources"]))
                .iter()
                .filter_map(|s| s.get("serviceAccountToken"))
                .map(move |t| (v, t))
        })
        .filter(|(_, t)| text(t, "audience").as_deref() == Some(terms.audience.as_str()))
        .collect();
    let [(volume, token)] = projected.as_slice() else {
        return Err(format!(
            "{instance}: {} projected tokens carry audience {}; one is the door's",
            projected.len(),
            terms.audience
        ));
    };
    if seq(volume.path(&["projected", "sources"])).len() != 1 {
        return Err(format!(
            "{instance}: the stage token's volume projects more than the token"
        ));
    }
    let lifetime = text(token, "expirationSeconds").unwrap_or_default();
    if lifetime != MAX_LIFETIME_SECS.to_string() {
        return Err(format!(
            "{instance}: the stage token is requested for {lifetime:?} seconds; the door believes {MAX_LIFETIME_SECS}"
        ));
    }
    let volume_name = text(volume, "name").unwrap_or_default();
    let mount = seq(container.get("volumeMounts"))
        .iter()
        .find(|m| text(m, "name").as_deref() == Some(volume_name.as_str()))
        .ok_or_else(|| {
            format!("{instance}: the {CONTAINER} container does not mount {volume_name}")
        })?;
    if text(mount, "readOnly").as_deref() != Some("true") {
        return Err(format!("{instance}: {volume_name} is mounted writable"));
    }
    let file = format!(
        "{}/{}",
        text(mount, "mountPath")
            .unwrap_or_default()
            .trim_end_matches('/'),
        text(token, "path").unwrap_or_default()
    );
    let token_env = token_file_env();
    if env.get(&token_env) != Some(&file) {
        return Err(format!(
            "{instance}: {token_env} is {:?}, and the kubelet writes the stage token at {file}",
            env.get(&token_env)
        ));
    }

    // Who else holds it: no other object in the instance projects the
    // audience or configures a door.
    for object in objects
        .iter()
        .filter(|o| !(o.kind == "Deployment" && o.name == "boss"))
    {
        let holds = object.root.walk().into_iter().any(|(k, n)| {
            (k == "audience" && n.str() == Some(terms.audience.as_str()))
                || (k == "name" && n.str().is_some_and(|s| s.starts_with("BOSS_BROKER_STAGE_")))
        });
        if holds {
            return Err(format!(
                "{instance}: {} also projects the stage audience or configures a door",
                object.label()
            ));
        }
    }
    Ok(format!("{instance}: {}", door.describe()))
}

#[test]
fn every_instance_turns_the_door_on_whole_for_its_own_workload() {
    let estate = estate();
    for instance in &estate.instances {
        match judge(estate, instance) {
            Ok(said) => println!("{said}"),
            Err(why) => panic!("{why}"),
        }
    }
}

/// A pin that cannot fail proves nothing: each edit below breaks one leg
/// of the agreement and must be refused by name.
#[test]
fn each_disagreement_is_refused_by_name() {
    let estate = estate();
    let prod = estate.source.clone();
    let label = format!("{prod}/boss.yaml");
    for (from, to, named) in [
        ("expirationSeconds: 600", "expirationSeconds: 3600", "3600"),
        (
            "audience: boss-credential-broker-stage,",
            "audience: boss-credential-broker-stage-2,",
            "projected tokens carry audience",
        ),
        (
            "{name: BOSS_BROKER_STAGE_TOKEN_FILE, value: /etc/boss/broker-stage/token}",
            "{name: BOSS_BROKER_STAGE_TOKEN_FILE, value: /etc/boss/broker-stage/other}",
            "the kubelet writes the stage token at",
        ),
        (
            "{name: BOSS_BROKER_STAGE_KEY_SET, value: \"https://kubernetes.default.svc/openid/v1/jwks\"}",
            "{name: BOSS_BROKER_STAGE_KEY_SET, value: \"http://kubernetes.default.svc/openid/v1/jwks\"}",
            "the door is not on",
        ),
        (
            "{name: BOSS_BROKER_STAGE_NAMESPACE, valueFrom: {fieldRef: {fieldPath: metadata.namespace}}}",
            "{name: BOSS_BROKER_STAGE_NAMESPACE, value: boss-elsewhere}",
            "the door expects workload boss-elsewhere/",
        ),
        (
            "{name: broker-stage-token, mountPath: /etc/boss/broker-stage, readOnly: true}",
            "{name: broker-stage-token, mountPath: /etc/boss/broker-stage}",
            "mounted writable",
        ),
    ] {
        let edited = estate.edited(&label, from, to);
        match judge(&edited, &prod) {
            Ok(said) => panic!("the edit {from:?} -> {to:?} was not refused: {said}"),
            Err(why) => assert!(why.contains(named), "{from:?}: {why}"),
        }
    }
}
