//! The production binary builds the commerce surfaces with the REAL
//! policy engine (backlog 54bf2e1e, 2026-09-26; 2b49ab60, 2026-09-27).
//!
//! `CommerceApiState.policy` and `agreements_router`'s policy are
//! required clients since 2b49ab60, so the type system already refuses
//! a surface with none — the old `policy: None`, which the invoice gate
//! read as "allow", cannot be written any more. What the type cannot
//! refuse is a binary handed an allow-all test client:
//! `PermissivePolicyClient` or `FakePolicyClient` would compile, and
//! would open every commerce write exactly as `None` did. This reads the
//! binary's source, because the wiring is the one place a port-level
//! test cannot reach — the shape of the ledger's
//! `the_ledger_api_wires_policy.rs`.

const BINARY: &str = include_str!("../src/bin/boss_commerce_api.rs");

fn code_lines() -> impl Iterator<Item = &'static str> {
    BINARY.lines().filter(|l| !l.trim_start().starts_with("//"))
}

#[test]
fn the_commerce_api_binary_wires_the_policy_engine() {
    let source = code_lines().collect::<Vec<_>>().join("\n");
    // The sim is admitted through the bypass wrapper, as people and
    // ledger do; a bare ReqwestPolicyClient would 403 the brewery sim.
    assert!(
        source.contains("SimBypassPolicyClient::from_env("),
        "boss_commerce_api.rs does not wire SimBypassPolicyClient::from_env(..)"
    );
    assert!(
        source.contains("ReqwestPolicyClient::new("),
        "boss_commerce_api.rs does not build a ReqwestPolicyClient"
    );
}

#[test]
fn the_commerce_api_binary_never_wires_an_allow_all_client() {
    let open: Vec<&str> = code_lines()
        .filter(|l| {
            ["PermissivePolicyClient", "FakePolicyClient", "policy: None"]
                .iter()
                .any(|c| l.contains(c))
        })
        .collect();
    assert!(
        open.is_empty(),
        "boss_commerce_api.rs wires a client that allows every write: {open:?}"
    );
}
