//! Root on boss-gcp fetches its own tree with a READ-ONLY forge token the
//! credential broker mints and only root holds (design-doc db018094,
//! question q1, David 2026-10-10: option (i); backlog ca356c79, whose
//! four boss-gcp units wait for that tree under decision (a) on design
//! fe6d1124).
//!
//! The forge's root tree needs no credential: it fetches from Forgejo's
//! repository on its own disk. boss-gcp has no such repository, and its
//! one checkout, /opt/boss, fetches as david through david's credential
//! helper — so root there had nothing of its own to fetch with. This is
//! that credential, and this file pins its declaration.
//!
//! Two rule files declare it, for the reason
//! `forge_host_checkout_token_rules.rs` gives: one rule has one trigger
//! and an off-host rotation needs two — the scope step starts it, and the
//! host's `delivered` step lets the revoke run. Both hand the SAME
//! declaration to the same handler, so it lives twice and is pinned equal
//! here (CLAUDE.md §9a). And the registry row the handler's rotation door
//! will demand is carried in the tree for the operator to publish
//! (infra/gcp/root-tree-token-declaration.toml), so it is a third copy of
//! the Secret's name, pinned here too.

use std::collections::BTreeSet;

use boss_dispatcher::rules::registry::{RawRegistry, RawRule, parse_raw_path};
use boss_jobs::credentials::seed::parse_credentials_toml;
use boss_testing::{dispatcher_rules_dir, repo_root};

const ROTATE: &str = "broker-rotates-the-boss-gcp-root-tree-token";
const DELIVERED: &str = "broker-revokes-the-boss-gcp-root-tree-token-on-delivery";
/// The repository-read proof: a read token is proven by reading the
/// repository it exists to fetch, which is exactly what it can do.
const HANDLER: &str = "credential.rotate.forgejo";
/// Every handler that mints a token of a forge user: instances of each
/// one's credentials are in the ledger a forgejo revoke reads.
const FORGEJO_HANDLERS: [&str; 2] = [HANDLER, "credential.rotate.forgejo-registry"];
const CREDENTIAL: &str = "boss-gcp-root-tree-token";
const DECLARATION: &str = "infra/gcp/root-tree-token-declaration.toml";
/// Where root on boss-gcp reads the value: a root-only directory under
/// /etc/boss, beside the machine token and the ops runner credential.
const CONSUMER_FILE: &str = "/etc/boss/root-tree/forge.token";

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
fn one_read_only_declaration_is_fired_by_the_scope_step_and_by_the_hosts_delivery() {
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
        assert_eq!(r.do_steps[0].handler, HANDLER, "{}", r.name);
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
        "read:repository",
        "David's answer is a READ-ONLY credential: root on the public edge may fetch the \
         tree and nothing else — no write:repository, no package scope"
    );
    assert_eq!(literal(rotate, "verify_repo"), "david/boss");
    assert_eq!(
        literal(rotate, "delivery"),
        "off-host",
        "boss-gcp reads no Secret; the value reaches it on a later pass, so the revoke \
         waits for the host's record"
    );
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
        want.contains("forge-host-checkout-token-*") && want.contains("boss-gcp"),
        "control: the forge checkout token and the conductor's are other declared holders: \
         {want:?}"
    );
    assert_eq!(
        got, want,
        "{ROTATE}'s `spare` must name every token another consumer is declared to hold"
    );
}

/// Any forge-user token rotation can address the same token ledger,
/// including mount-delivered credentials. A missing spare is a failure,
/// not a reason to skip the rule (review 7a693f47, F1).
#[test]
fn every_other_forgejo_rotation_spares_this_credential() {
    let reg = rules();
    let mine = format!("{CREDENTIAL}-*");
    let mut checked = 0;
    for r in &reg.rules {
        let Some(step) = r.do_steps.first() else {
            continue;
        };
        if !FORGEJO_HANDLERS.contains(&step.handler.as_str())
            || literal(r, "secret_name") == CREDENTIAL
        {
            continue;
        }
        checked += 1;
        assert!(
            literal(r, "spare").split(',').any(|s| s.trim() == mine),
            "{} may revoke a token of the forge user; its `spare` must name {mine}, or a \
             rotation of it could revoke the token root on boss-gcp fetches its tree with",
            r.name
        );
    }
    assert!(
        checked >= 5,
        "control: the dev rotation and checkout/registry pairs all spare; found {checked}"
    );
}

/// The registry row the rotation door demands, as the tree carries it for
/// the operator to publish (`boss tenant publish` in the instance
/// repository): it must parse as the registry's own loader parses it,
/// name the rule's Secret, and say where root on boss-gcp reads the value.
#[test]
fn the_declared_registry_row_is_the_rules_credential_and_names_roots_file() {
    let text = std::fs::read_to_string(repo_root().join(DECLARATION))
        .unwrap_or_else(|e| panic!("{DECLARATION}: {e}"));
    let rows = parse_credentials_toml(&text).unwrap_or_else(|e| panic!("{DECLARATION}: {e}"));
    assert_eq!(
        rows.len(),
        1,
        "one row, appended verbatim to seeds/credentials.toml"
    );
    let raw: toml::Value = toml::from_str(&text).expect("parses");
    assert_eq!(
        raw.as_table().map(|t| t.len()),
        Some(1),
        "nothing but the [[credential]] row"
    );
    let row = &rows[0];
    let reg = rules();
    let rotate = rule(&reg, ROTATE);
    assert_eq!(row.id, CREDENTIAL, "the rule's `when` names this id");
    assert_eq!(row.kind, "forgejo-access-token");
    assert_eq!(
        row.principal,
        format!("user {}", literal(rotate, "forge_user"))
    );
    assert_eq!(row.scopes, vec![literal(rotate, "scopes")]);
    assert!(
        row.storage_location.starts_with(&format!(
            "k8s Secret {}/{} key {}",
            literal(rotate, "secret_namespace"),
            literal(rotate, "secret_name"),
            literal(rotate, "secret_key")
        )),
        "the row says where the broker writes: {}",
        row.storage_location
    );
    assert!(
        row.storage_location.contains(CONSUMER_FILE)
            && row.storage_location.contains("root:root 0600"),
        "the row says where root on boss-gcp reads it, and that only root can: {}",
        row.storage_location
    );
    assert!(
        row.consumers
            .iter()
            .any(|c| c.location.contains(CONSUMER_FILE) && c.location.contains("boss-gcp")),
        "a consumer names root's file on boss-gcp: {:?}",
        row.consumers
    );
    assert!(
        row.notes.contains("pending:")
            && row
                .notes
                .contains("nothing reads it until the later cars of db018094"),
        "the published row must say delivery and the consumer are pending: {:?}",
        row.notes
    );
    for forbidden in ["value", "token =", "secret ="] {
        assert!(
            !text.contains(&format!("\n{forbidden}")),
            "a declaration never carries `{forbidden}`"
        );
    }
}
