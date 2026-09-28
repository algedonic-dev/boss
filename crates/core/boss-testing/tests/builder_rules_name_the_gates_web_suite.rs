//! The builder rules must name every web check the gate's web-suite
//! phase runs, so a frontend car runs what will judge it.
//!
//! MEASURED 2026-09-22 (backlog 3566f5b4). A car's first gate went red
//! on six checks, and FIVE of them were mocked specs under
//! `apps/web/tests/mocked/`. Rule 10 mentioned mocked specs only as a
//! reason to have node_modules and never gave the command; the command
//! a builder does reach for, `bun test src/`, is `test:unit` and reads
//! nothing under `tests/`. Two minutes of `bun run test:mocked` would
//! have named all five.
//!
//! CLAUDE.md §9a: the roster is not retyped here. `infra/gate.sh`'s
//! web-suite check is the definition, and this test reads the `bun run
//! <script>` names out of it — so a phase added there and not written
//! into the rules fails HERE, by name, rather than on a builder's
//! first gate.
//!
//! AND THE SIMULATOR'S CHAIN (backlog 5796d7ea, 2026-09-26). The web
//! phase opened on an `apps/simulator` change and then checked nothing
//! of it, so a broken simulator was green everywhere. The gate now runs
//! a `simulator` check — typecheck, unit, build — and rule 10 carries
//! that chain too. Both chains are held VERBATIM: the rules tell a
//! builder to paste each into a script, so a rule that names the right
//! scripts in the wrong directory or order is still the wrong command.

use boss_testing::repo_root;

/// The `bash -c '<chain>'` body of the web-phase check whose name
/// starts with `name`, read out of `infra/gate.sh`.
fn gate_chain(name: &str) -> String {
    let gate = std::fs::read_to_string(repo_root().join("infra/gate.sh")).expect("infra/gate.sh");
    let needle = format!("check \"{name}");
    let line = gate
        .lines()
        .find(|l| l.trim_start().starts_with(&needle))
        .unwrap_or_else(|| panic!("infra/gate.sh runs a `{name}` check"));
    let body = line
        .split_once("bash -c '")
        .and_then(|(_, rest)| rest.split_once('\''))
        .map(|(body, _)| body.to_string())
        .unwrap_or_else(|| panic!("the `{name}` check must be `bash -c '<chain>'`: {line}"));
    assert!(
        body.starts_with("cd "),
        "the `{name}` chain must start by naming its directory: {body}"
    );
    body
}

fn gate_web_suite_scripts() -> Vec<String> {
    let gate = std::fs::read_to_string(repo_root().join("infra/gate.sh")).expect("infra/gate.sh");
    let line = gate
        .lines()
        .find(|l| l.contains("check \"web-suite"))
        .expect("infra/gate.sh runs a web-suite check");
    let scripts: Vec<String> = line
        .split("bun run ")
        .skip(1)
        .map(|rest| {
            rest.split([' ', '\'', '"', '&'])
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        !scripts.is_empty(),
        "the web-suite check must name its scripts as `bun run <name>`: {line}"
    );
    scripts
}

#[test]
fn the_rules_name_every_script_the_gates_web_suite_runs() {
    let rules =
        std::fs::read_to_string(repo_root().join("infra/platform/documents/builder-rules.md"))
            .expect("the builder rules are readable");
    for script in gate_web_suite_scripts() {
        assert!(
            rules.contains(&format!("bun run {script}")),
            "the builder rules must tell a frontend car to run `bun run {script}` — \
             the gate's web-suite phase does, and five mocked specs redded a car \
             on 2026-09-22 because the rules never named it"
        );
    }
}

/// Each web chain the rules hand a builder is the gate's own, verbatim,
/// with the gate's repo-relative `cd` spelled from the worktree.
#[test]
fn the_rules_carry_each_web_chain_verbatim() {
    let rules =
        std::fs::read_to_string(repo_root().join("infra/platform/documents/builder-rules.md"))
            .expect("the builder rules are readable");
    for name in ["web-suite", "simulator"] {
        let chain = gate_chain(name);
        let spelled = chain.replacen("cd ", "cd <your worktree>/", 1);
        assert!(
            rules.contains(&spelled),
            "the builder rules must hand a web car the gate's `{name}` chain verbatim, \
             as `{spelled}` — it is what will judge the car"
        );
    }
}
