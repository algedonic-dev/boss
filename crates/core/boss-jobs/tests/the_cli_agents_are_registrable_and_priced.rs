//! Codex and Gemini sign as themselves: `infra/dev/cli/agents.json`
//! declares them, and this file holds the declaration to what the
//! registry will take (backlog 5840c068, David 2026-10-01).
//!
//! WHAT WAS MEASURED (run 3f46595c). `agents.default_model` is
//! `TEXT NOT NULL REFERENCES agent_rate_card (model)` (schema
//! 20260915212644), and the live rate card held only Claude models, so
//! no row for a Codex or Gemini session could be inserted at all. And an
//! agent row holding a ROLE is put on the dispatcher's roster by
//! `roster_union`, while `eligible_candidates` / `capability_executor`
//! never read `max_concurrent_runs` — a role would let the dispatcher
//! assign these CLIs steps nothing launches them for. So each holds no
//! role: reachable by id, never by a role audience.
//!
//! The file is not seeded by anything; the operator POSTs it to
//! `/api/agents/batch?mode=insert-if-absent` after it lands. A refusal
//! there would be the first anyone heard of a bad row, so the checks
//! the batch door can make without a database run here, at the gate:
//! `validate_agent` (the door's own shape check), and `default_model`
//! against the rate card's compiled model set — the FK the insert
//! would otherwise trip.
//!
//! THE MODEL LIVES TWICE (CLAUDE.md §9a). The CLI's own config says
//! which model a session runs; the registry row says which model its
//! runs are priced as. They cannot collapse — one is read by codex /
//! gemini, the other by the jobs API — so the second test pins them
//! equal and names the side that drifted.

use boss_jobs::agent_spec::known_models;
use boss_jobs::agents::{AgentInput, validate_agent};
use std::path::PathBuf;

fn cli_dir() -> PathBuf {
    boss_testing::repo_root().join("infra/dev/cli")
}

fn declared() -> Vec<AgentInput> {
    let path = cli_dir().join("agents.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| {
        panic!(
            "{} is the batch door's body, a bare JSON array of AgentInput: {e}",
            path.display()
        )
    })
}

fn agent(id: &str) -> AgentInput {
    declared()
        .into_iter()
        .find(|a| a.id == id)
        .unwrap_or_else(|| panic!("infra/dev/cli/agents.json declares {id}"))
}

#[test]
fn each_cli_agent_passes_the_batch_doors_checks_and_names_a_priced_model() {
    let rows = declared();
    let mut ids: Vec<&str> = rows.iter().map(|a| a.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        ["agent-codex", "agent-gemini"],
        "the file declares exactly the two CLI agents"
    );

    for (id, alias, display) in [
        ("agent-codex", "codex@algedonic.dev", "Codex (engineering)"),
        (
            "agent-gemini",
            "gemini@algedonic.dev",
            "Gemini (engineering)",
        ),
    ] {
        let a = agent(id);
        if let Err(why) = validate_agent(&a) {
            panic!("{id} would be refused at the batch door: {why}");
        }
        assert!(
            known_models().iter().any(|m| m == &a.default_model),
            "{id}: default_model `{}` is not a rate-card row, so the agents FK refuses the \
             insert — known models: {}",
            a.default_model,
            known_models().join(", ")
        );
        assert_eq!(a.aliases, [alias], "{id}: its login is its one alias");
        assert_eq!(a.display_name, display, "{id}: display name");
        assert_eq!(
            a.role, None,
            "{id}: a role puts it on the dispatcher roster (roster_union), which would assign \
             it steps it cannot claim — reachable by id only"
        );
        assert_eq!(a.department.as_deref(), Some("it"), "{id}: department");
        assert!(
            a.max_concurrent_runs.is_some_and(|n| n > 0),
            "{id}: a concurrency cap is declared, and above zero (zero refuses every claim)"
        );
        assert!(
            a.hourly_budget_usd_micros.is_some_and(|n| n > 0),
            "{id}: an hourly budget is declared"
        );
    }
}

#[test]
fn each_cli_config_pins_the_model_its_agent_row_is_priced_as() {
    let codex_path = cli_dir().join("codex-config.toml");
    let codex: toml::Value = toml::from_str(
        &std::fs::read_to_string(&codex_path)
            .unwrap_or_else(|e| panic!("{} is readable: {e}", codex_path.display())),
    )
    .expect("codex-config.toml parses");
    let codex_model = codex
        .get("model")
        .and_then(toml::Value::as_str)
        .expect("codex-config.toml pins `model = \"…\"` — unpinned, the CLI picks its own");
    assert_eq!(
        codex_model,
        agent("agent-codex").default_model,
        "codex-config.toml runs one model and agent-codex is priced as another"
    );

    let gemini_path = cli_dir().join("gemini-settings.json");
    let gemini: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&gemini_path)
            .unwrap_or_else(|e| panic!("{} is readable: {e}", gemini_path.display())),
    )
    .expect("gemini-settings.json parses");
    let gemini_model = gemini
        .pointer("/model/name")
        .and_then(serde_json::Value::as_str)
        .expect("gemini-settings.json pins model.name — unpinned, the CLI routes on its own");
    assert_eq!(
        gemini_model,
        agent("agent-gemini").default_model,
        "gemini-settings.json runs one model and agent-gemini is priced as another"
    );
}
