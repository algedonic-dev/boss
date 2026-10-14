//! The depositors the asked-row alarm excuses are registered automations
//! (backlog d653e090).
//!
//! `an-ask-from-a-stranger-is-read-hourly` names, in its `depositors`
//! arg, the ids whose asks about a machine-token value are expected; the
//! handler (`machine.gate.asked.alarm`) names nobody but the dispatcher
//! itself. That list is a second mention of facts that live elsewhere —
//! the automation registry's rows, and the id each depositing script
//! signs as — and CLAUDE.md §9a asks for a test where a fact lives twice.
//!
//! What a drift would cost, either way: an id excused here that no row
//! registers is a name anyone may assert and never be listed under; a
//! script that changes the id it signs as is alarmed on every deposit.

use boss_dispatcher::rules::registry::parse_raw_path;
use boss_dispatcher_handlers::handlers::machine_gate_asked_alarm::{
    DEPOSITORS_ARG, DISPATCHER, HANDLER,
};
use boss_testing::{dispatcher_rules_dir, repo_root};

const RULE: &str = "an-ask-from-a-stranger-is-read-hourly";

/// The ids the shipped rule excuses, as the handler splits them.
fn excused() -> Vec<String> {
    let raw = parse_raw_path(dispatcher_rules_dir()).expect("parse the shipped rule registry");
    let rule = raw
        .rules
        .iter()
        .find(|r| r.name == RULE)
        .unwrap_or_else(|| panic!("the registry holds no rule {RULE}"));
    assert!(rule.schedule.is_some(), "{RULE} is a clock rule");
    let [step] = rule.do_steps.as_slice() else {
        panic!("{RULE} runs exactly one handler: {:?}", rule.do_steps);
    };
    assert_eq!(step.handler, HANDLER);
    let arg = step
        .args
        .get(DEPOSITORS_ARG)
        .unwrap_or_else(|| panic!("{RULE} passes no `{DEPOSITORS_ARG}`"));
    // The arg is an expression: a string literal, quoted.
    let list = arg
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or_else(|| panic!("`{DEPOSITORS_ARG}` is one quoted string literal: {arg}"));
    list.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn tree(path: &str) -> String {
    std::fs::read_to_string(repo_root().join(path)).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

#[test]
fn every_excused_depositor_is_a_registered_automation_a_script_signs_as() {
    let ids = excused();
    assert!(!ids.is_empty(), "{RULE} excuses no depositor at all");
    // Each excused id, with the script that asks the gate under it.
    let signers = [
        (
            "automation:machine-token-deposit",
            "infra/forge/machine-token-deposit.sh",
        ),
        (
            "automation:machine-token-effect",
            "infra/estate/machine-token-effect.sh",
        ),
    ];
    for id in &ids {
        let name = id
            .strip_prefix("automation:")
            .unwrap_or_else(|| panic!("{id}: a depositor is an `automation:` id"));
        let row = tree(&format!("infra/platform/automations/{name}.toml"));
        assert!(
            row.contains(&format!("id = \"{id}\"")),
            "{id} is excused by {RULE} and infra/platform/automations/{name}.toml does not \
             register it"
        );
        let script = signers
            .iter()
            .find(|(signer, _)| signer == id)
            .map(|(_, script)| *script)
            .unwrap_or_else(|| {
                panic!(
                    "{id} is excused by {RULE} and this pin names no script that asks the gate \
                     as it: name the script here, beside the rule edit"
                )
            });
        let text = tree(script);
        assert!(
            text.contains(&format!("\"id\":\"{id}\""))
                && text.contains("/api/machine-gate/accepts"),
            "{script} no longer asks /api/machine-gate/accepts signed as {id}"
        );
    }
    // The dispatcher is the handler's own to know, by id AND loopback;
    // named here it would be excused from any address.
    assert!(
        !ids.iter().any(|id| id == DISPATCHER),
        "{RULE} names {DISPATCHER} as a depositor, which excuses that id from every peer"
    );
}
