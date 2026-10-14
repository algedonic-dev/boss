//! The forge's registry push token is minted by the broker and its old
//! instance revoked only after the forge host records delivery (design
//! c8502e17, question image-build, David 2026-10-07; backlog 683d7fe9).
//!
//! Two rule files declare it, for the reason
//! `forge_host_checkout_token_rules.rs` gives: one rule has one trigger
//! and an off-host rotation needs two. Both hand the SAME declaration to
//! the same handler, so the declaration lives twice and is pinned equal
//! here (CLAUDE.md §9a).
//!
//! What this file holds beyond the twin: the KIND. A registry token is
//! proved by a push, so its rules name the registry handler and an image
//! — never the repository handler, whose proof a `write:package` token
//! cannot pass, and never a `verify_repo`, which the registry handler
//! does not read. And its `spare` is every OTHER declared holder of a
//! token of the same forge user, by the same derivation the checkout
//! token's pin uses, so neither credential's rotation can revoke the
//! other's instance.

use std::collections::BTreeSet;

use boss_dispatcher::rules::registry::{RawRegistry, RawRule, parse_raw_path};
use boss_testing::{dispatcher_rules_dir, repo_root};

const ROTATE: &str = "broker-rotates-the-forge-registry-push-token";
const DELIVERED: &str = "broker-revokes-the-forge-registry-push-token-on-delivery";
const HANDLER: &str = "credential.rotate.forgejo-registry";
const FORGEJO_HANDLERS: [&str; 2] = ["credential.rotate.forgejo", HANDLER];
const CREDENTIAL: &str = "forge-registry-push-token";

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
    let rotate = rule(&reg, ROTATE);
    let delivered = rule(&reg, DELIVERED);
    assert_eq!(
        rotate.on_event.as_deref(),
        Some("step.done.credential-rotation")
    );
    assert_eq!(
        delivered.on_event.as_deref(),
        Some("step.done.credential-delivery"),
        "the second firing is the host's `delivered` step, and only it"
    );
    let when = format!("subject_id = \"{CREDENTIAL}\"");
    for r in [rotate, delivered] {
        assert_eq!(
            r.when.as_deref(),
            Some(when.as_str()),
            "{} must fire for this credential's packets only",
            r.name
        );
        assert_eq!(r.do_steps.len(), 1, "{}: one handler", r.name);
        assert_eq!(
            r.do_steps[0].handler, HANDLER,
            "{}: a package token is proved by a push, which only the registry handler makes",
            r.name
        );
        assert!(
            !r.do_steps[0].args.contains_key("verify_repo"),
            "{}: the registry handler reads `verify_image`; a `verify_repo` beside it would \
             read as a proof that is never made",
            r.name
        );
    }
    assert_eq!(
        rotate.do_steps[0].args, delivered.do_steps[0].args,
        "{ROTATE} and {DELIVERED} must hand the handler one declaration"
    );

    assert_eq!(literal(rotate, "secret_namespace"), "boss");
    assert_eq!(literal(rotate, "secret_name"), CREDENTIAL);
    assert_eq!(literal(rotate, "secret_key"), "token");
    assert_eq!(literal(rotate, "forge_user"), "david");
    assert_eq!(
        literal(rotate, "scopes"),
        "write:package",
        "a push token and nothing wider: it must not be able to write a repository"
    );
    assert_eq!(
        literal(rotate, "delivery"),
        "off-host",
        "the host takes the value on its next pass, so the revoke waits for its record"
    );
}

/// The image the proof pushes to is the image the converge pushes: the
/// registry host, then `<owner>/boss`, with the owner
/// infra/forge/forge-defaults.sh gives every forge script.
#[test]
fn the_proving_image_is_the_image_the_converge_pushes() {
    let defaults = std::fs::read_to_string(repo_root().join("infra/forge/forge-defaults.sh"))
        .expect("the forge defaults");
    let owner = defaults
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("FORGE_OWNER=\"${BOSS_FORGE_OWNER:-")
                .and_then(|rest| rest.strip_suffix("}\""))
        })
        .expect("forge-defaults.sh declares FORGE_OWNER with a default");
    assert!(
        defaults.contains("REGISTRY=\"$BOSS_FORGE_REGISTRY_HOST/$FORGE_OWNER/boss\""),
        "control: the converge's image is <registry host>/<owner>/boss"
    );
    let reg = rules();
    assert_eq!(
        literal(rule(&reg, ROTATE), "verify_image"),
        format!("{owner}/boss")
    );
    assert_eq!(literal(rule(&reg, ROTATE), "forge_user"), owner);
}

#[test]
fn the_spared_tokens_are_every_other_declared_holder() {
    let reg = rules();
    let got: BTreeSet<String> = literal(rule(&reg, ROTATE), "spare")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let inventory: toml::Value = toml::from_str(
        &std::fs::read_to_string(repo_root().join("infra/platform/forge-tokens.toml"))
            .expect("the forge token inventory"),
    )
    .expect("the inventory parses");
    let mut want: BTreeSet<String> = inventory["token"]
        .as_array()
        .expect("[[token]] rows")
        .iter()
        .filter(|t| t["consumer"].as_str() != Some("UNKNOWN"))
        .map(|t| t["name"].as_str().expect("a name").to_string())
        .collect();
    for r in &reg.rules {
        let Some(step) = r.do_steps.first() else {
            continue;
        };
        if !FORGEJO_HANDLERS.contains(&step.handler.as_str()) {
            continue;
        }
        let secret = literal(r, "secret_name");
        if secret != CREDENTIAL {
            want.insert(format!("{secret}-*"));
        }
    }
    assert!(
        want.contains("forge-host-checkout-token-*"),
        "control: the forge host's checkout token is another forgejo broker credential: {want:?}"
    );
    assert_eq!(
        got, want,
        "{ROTATE}'s `spare` must name every token another consumer is declared to hold \
         (infra/platform/forge-tokens.toml rows with a consumer, and every other forgejo \
         broker credential's instances)"
    );
}
