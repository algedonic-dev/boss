//! The ops runner's credential is minted by the broker, mounted on the
//! jobs API whole, and promoted only after its host records delivery
//! (design f623e425 Q1, option A; backlog 1e50e66b).
//!
//! One credential, four places that must agree (CLAUDE.md §9a), each
//! pinned here against the others rather than retyped:
//!
//! - the two rule files — the scope step stages and the host's
//!   `delivered` step promotes, so the declaration lives twice and a
//!   promotion that read another Secret or another host's slots would
//!   judge the host's delivery against the wrong value;
//! - the host they name — an estate node carrying the `ops-runner`
//!   role, the only hosts whose runner presents a credential;
//! - the Secret they write — the one boss.yaml mounts for the jobs
//!   API, at the directory the door reads by default, as the WHOLE
//!   Secret (a `subPath` or an `items:` subset is never refreshed, and
//!   a slot for a new host must appear without an edit — review of
//!   8e5de104, finding 4(a)) and `optional`, because the door refuses
//!   nothing and the system of record must boot without it.

use boss_dispatcher::rules::registry::{RawRegistry, RawRule, parse_raw_path};
use boss_testing::{dispatcher_rules_dir, repo_root};

const STAGE: &str = "broker-rotates-the-forge-ops-runner-credential";
const PROMOTE: &str = "broker-promotes-the-forge-ops-runner-credential-on-delivery";
const HANDLER: &str = "credential.rotate.ops-runner";

fn rules() -> RawRegistry {
    parse_raw_path(dispatcher_rules_dir()).expect("parse the rule directory")
}

fn rule<'a>(reg: &'a RawRegistry, name: &str) -> &'a RawRule {
    reg.rules
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("rule {name} is in the authored directory"))
}

/// An arg's expr source `"\"boss\""` as the bare literal `boss`.
fn literal(rule: &RawRule, key: &str) -> String {
    let raw = rule.do_steps[0]
        .args
        .get(key)
        .unwrap_or_else(|| panic!("{} declares no `{key}`", rule.name));
    raw.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or_else(|| panic!("{}: `{key}` is not a string literal: {raw}", rule.name))
        .to_string()
}

#[test]
fn one_declaration_is_fired_by_the_scope_step_and_by_the_hosts_delivery() {
    let reg = rules();
    let stage = rule(&reg, STAGE);
    let promote = rule(&reg, PROMOTE);
    assert_eq!(
        stage.on_event.as_deref(),
        Some("step.done.credential-rotation")
    );
    assert_eq!(
        promote.on_event.as_deref(),
        Some("step.done.credential-delivery"),
        "the promotion fires on the host's `delivered` step, and only it"
    );
    let credential = literal(stage, "credential_id");
    let when = format!("subject_id = \"{credential}\"");
    for r in [stage, promote] {
        assert_eq!(
            r.when.as_deref(),
            Some(when.as_str()),
            "{} must fire for this credential's packets only",
            r.name
        );
        assert_eq!(r.do_steps.len(), 1, "{}: one handler", r.name);
        assert_eq!(r.do_steps[0].handler, HANDLER, "{}", r.name);
    }
    assert_eq!(
        stage.do_steps[0].args, promote.do_steps[0].args,
        "{STAGE} and {PROMOTE} must hand the handler one declaration"
    );
    let host = literal(stage, "host");
    assert_eq!(literal(stage, "secret_namespace"), "boss");
    assert_eq!(literal(stage, "secret_name"), "ops-runner-credential");
    assert_eq!(
        credential,
        format!("ops-runner-credential-{host}"),
        "one registry id per host, named after it"
    );
}

/// Every host an `ops-runner` rule declares is an estate node whose roles
/// include `ops-runner` — read from infra/estate/estate.toml, the one
/// definition of which hosts answer ops-requests.
#[test]
fn every_declared_host_is_an_ops_runner_in_the_estate() {
    let estate: toml::Value = toml::from_str(
        &std::fs::read_to_string(repo_root().join("infra/estate/estate.toml"))
            .expect("the estate registry"),
    )
    .expect("estate.toml parses");
    let runners: Vec<&str> = estate["node"]
        .as_array()
        .expect("[[node]] rows")
        .iter()
        .filter(|n| {
            n.get("roles")
                .and_then(toml::Value::as_array)
                .is_some_and(|r| r.iter().any(|r| r.as_str() == Some("ops-runner")))
        })
        .filter_map(|n| n.get("id").and_then(toml::Value::as_str))
        .collect();
    assert!(
        runners.contains(&"forge"),
        "control: the forge runs an ops runner: {runners:?}"
    );
    let reg = rules();
    let declared: Vec<String> = reg
        .rules
        .iter()
        .filter(|r| r.do_steps.first().is_some_and(|s| s.handler == HANDLER))
        .map(|r| literal(r, "host"))
        .collect();
    assert!(
        !declared.is_empty(),
        "control: the forge's rules declare a host"
    );
    for host in &declared {
        assert!(
            runners.contains(&host.as_str()),
            "{HANDLER} is declared for host {host}, which the estate does not list as an \
             ops-runner: {runners:?}"
        );
    }
}

/// The Secret every rule writes is the one the jobs API mounts, whole,
/// optional, read-only, at the directory the door reads by default.
#[test]
fn the_jobs_api_mounts_the_brokers_whole_secret_where_the_door_reads() {
    let reg = rules();
    let secret = literal(rule(&reg, STAGE), "secret_name");
    let yaml = std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss.yaml"))
        .expect("boss.yaml");
    let lines: Vec<&str> = yaml.lines().collect();

    // The volume: `- name: <vol>` then, within its block, the Secret.
    let at = lines
        .iter()
        .position(|l| l.trim() == format!("secretName: {secret}"))
        .unwrap_or_else(|| panic!("boss.yaml mounts no volume of Secret {secret}"));
    let volume = lines[..at]
        .iter()
        .rev()
        .find_map(|l| l.trim().strip_prefix("- name: "))
        .expect("the volume's name");
    let block: Vec<&str> = lines[at..]
        .iter()
        .take_while(|l| !l.trim_start().starts_with("- ") && !l.trim_start().starts_with('#'))
        .map(|l| l.trim())
        .collect();
    assert!(
        block.contains(&"optional: true"),
        "{volume}: the door refuses nothing, so the system of record boots without the \
         Secret: {block:?}"
    );
    assert!(
        !block.iter().any(|l| l.starts_with("items:")),
        "{volume}: the WHOLE Secret, never an items: subset — a new host's slot must appear \
         without an edit: {block:?}"
    );

    // Every mount of that volume.
    let mounts: Vec<&str> = lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| l.starts_with(&format!("- {{name: {volume},")))
        .collect();
    assert_eq!(
        mounts.len(),
        1,
        "{volume} is mounted once, on the container running the jobs API: {mounts:?}"
    );
    let mount = mounts[0];
    assert!(
        mount.contains(&format!(
            "mountPath: {},",
            boss_jobs::runner_credential::DEFAULT_DIR
        )),
        "{volume} must be mounted at the door's default directory: {mount}"
    );
    assert!(mount.contains("readOnly: true"), "{mount}");
    assert!(
        !mount.contains("subPath"),
        "a subPath mount is never refreshed by kubelet (review of 8e5de104, 4(a)): {mount}"
    );
}
