//! The gate-runner declares its load to the web suites, and the receipt
//! carries what the web suites took (backlog ebb750cd).
//!
//! MEASURED 2026-09-28. Two PR trains were disassembled by web-suite
//! reds that were not code failures, both while three concurrent gates
//! each built cargo 20-wide on w-1's one NVMe. 00:01Z:
//! `scripts/bunfig-keys-take-effect.test.ts` hit its 30 s budget at
//! 30 155 ms — 5.0 s alone on the dev pod. 03:11Z: a Crew Board
//! `toBeVisible` against the 15 s expect budget took 27.6 s — 658 / 676 /
//! 624 ms on the dev pod, same tree. Each learned its load factor only
//! by a reproduction on an idle pod, because the receipt kept the
//! failure and not one duration.
//!
//! So: `infra/gate-runner/run.sh` exports the web tree's GATE_LOAD_ENV
//! as `1` before it runs the gate, and `apps/web/src/dev-load.ts` scales
//! every web budget by GATE_LOAD_SCALE under it; `infra/gate.sh` names a
//! timings file (TIMINGS_ENV) for its web phase, the unit runner and the
//! mocked suite's reporter append one line per file / test to it, and
//! the receipt carries the slowest of each suite as `web_timings`.
//!
//! The two environment names live in the web tree and are SPELLED in
//! run.sh and gate.sh — a fact that lives twice gets an equality test
//! (CLAUDE.md §9a), and a misspelling on either side is silence: the
//! suites would run on quiet budgets, or write no timings, and say
//! nothing. The receipt's block is lifted out of gate.sh and RUN, as
//! `the_gate_keeps_a_failed_specs_error_context.rs` does its own.

use boss_testing::repo_root;
use serde_json::Value;
use std::process::Command;

const OPEN: &str = "# --- web timings (begin) ---";
const CLOSE: &str = "# --- web timings (end) ---";

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// A file with comment lines dropped: the pins are about what runs.
fn printed(rel: &str) -> String {
    read(rel)
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `export const <NAME> = '<value>';` out of apps/web/src/dev-load.ts.
fn web_const(name: &str) -> String {
    let src = read("apps/web/src/dev-load.ts");
    let decl = format!("export const {name} = ");
    let at = src
        .find(&decl)
        .unwrap_or_else(|| panic!("apps/web/src/dev-load.ts declares no `{decl}`"));
    let rest = &src[at + decl.len()..];
    let end = rest
        .find(';')
        .unwrap_or_else(|| panic!("`{decl}` is not terminated"));
    rest[..end].trim().trim_matches('\'').to_string()
}

fn block() -> String {
    let src = read("infra/gate.sh");
    let start = src
        .find(OPEN)
        .unwrap_or_else(|| panic!("infra/gate.sh has no `{OPEN}` block"));
    let end = src[start..]
        .find(CLOSE)
        .unwrap_or_else(|| panic!("infra/gate.sh's web timings block is not closed"));
    src[start..start + end].to_string()
}

#[test]
fn the_runner_declares_the_load_the_web_tree_reads_before_it_runs_the_gate() {
    let env = web_const("GATE_LOAD_ENV");
    assert!(env.starts_with("BOSS_"), "GATE_LOAD_ENV read as `{env}`");
    let sh = printed("infra/gate-runner/run.sh");
    let export = format!("export {env}=1");
    let declared = sh.find(&export).unwrap_or_else(|| {
        panic!(
            "infra/gate-runner/run.sh does not `{export}` — apps/web/src/dev-load.ts reads that name, \
             so the gate's web suites would run on quiet budgets and say nothing"
        )
    });
    let gate = sh
        .find("./infra/gate.sh")
        .expect("run.sh runs ./infra/gate.sh");
    assert!(
        declared < gate,
        "run.sh declares {env} only after it has run the gate"
    );
}

#[test]
fn the_gate_names_the_timings_file_the_web_tree_writes_before_its_first_web_check() {
    let env = web_const("TIMINGS_ENV");
    let b = block();
    assert!(
        b.contains(&format!("export {env}=")),
        "gate.sh's web timings block does not export {env} — the name apps/web/src/dev-load.ts reads"
    );
    let load = web_const("GATE_LOAD_ENV");
    assert!(
        b.contains(&format!("${{{load}:-}}")),
        "the receipt's web_timings does not record {load}, the load the runner declared"
    );
    let sh = printed("infra/gate.sh");
    let phase = sh
        .find("if [ \"$AUTO\" -eq 0 ] || [ \"$(web_touched)\" = \"yes\" ]; then")
        .expect("gate.sh opens its web phase on web_touched");
    let begin = sh[phase..]
        .find("web_timings_begin")
        .map(|i| phase + i)
        .expect("the web phase calls web_timings_begin");
    let install = sh
        .find("check \"web install\"")
        .expect("gate.sh has a web install check");
    assert!(
        begin < install,
        "web_timings_begin must run before the web phase's first check"
    );
    assert!(
        sh.contains("\"web_timings\": $(web_timings_json),"),
        "the receipt does not carry web_timings"
    );
}

/// Run the lifted block: `setup` is shell run before `web_timings_json`.
fn receipt_json(tag: &str, setup: &str) -> Value {
    let tree = boss_testing::scratch_dir(&format!("gate-web-timings-{tag}"));
    let script = tree.join("run.sh");
    boss_testing::write_file(
        &script,
        &format!(
            "set -uo pipefail\n\
             GATE_RECEIPT=receipt.json\n\
             {block}\n\
             {setup}\n\
             web_timings_json\n",
            block = block()
        ),
    );
    let out = Command::new("bash")
        .arg(&script)
        .current_dir(&tree)
        .env_remove("BOSS_WEB_GATE_LOAD")
        .env_remove("BOSS_WEB_TIMINGS")
        .output()
        .expect("bash runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "the block failed: {}\n{stdout}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {stdout}"))
}

#[test]
fn a_gate_whose_web_phase_did_not_run_says_null() {
    assert_eq!(receipt_json("unrun", ""), Value::Null);
}

#[test]
fn the_receipt_carries_each_suites_slowest_slowest_first_with_the_declared_load() {
    // Ten mocked tests out of order (the Crew Board's 27.6 s among
    // them), three unit files, a title carrying a quote and a backslash,
    // and a line whose time is not a number — the file is written by
    // another program, so the block trusts none of it.
    let mut rows: Vec<String> = (1..=9)
        .map(|i| {
            format!(
                "{}\tmocked\tpassed\ttests/mocked/s{i}.mocked.spec.ts:1 › t{i}",
                i * 100
            )
        })
        .collect();
    rows.insert(
        4,
        "27612\tmocked\tfailed\ttests/mocked/it-department-map-stations.mocked.spec.ts:375 › draws its \"Crew\\Board\""
            .to_string(),
    );
    rows.push("30155\tunit:web\tfailed\t./scripts/bunfig-keys-take-effect.test.ts".to_string());
    rows.push("41\tunit:web\tpassed\t./src/router.test.ts".to_string());
    rows.push("oops\tunit:web\tpassed\t./src/garbled.test.ts".to_string());
    let body = rows.join("\n") + "\n";
    let setup = format!(
        "web_timings_begin\n\
         [ \"$BOSS_WEB_TIMINGS\" = \"$PWD/receipt.web-timings.tsv\" ] || {{ echo \"named $BOSS_WEB_TIMINGS\" >&2; exit 1; }}\n\
         [ -f \"$BOSS_WEB_TIMINGS\" ] && [ ! -s \"$BOSS_WEB_TIMINGS\" ] || {{ echo 'not started empty' >&2; exit 1; }}\n\
         printf '%s' '{}' > \"$BOSS_WEB_TIMINGS\"\n\
         export BOSS_WEB_GATE_LOAD=1",
        body.replace('\'', "'\\''")
    );
    let got = receipt_json("slowest", &setup);

    assert_eq!(got["gate_load"], "1", "{got}");
    let n = got["slowest_n"].as_u64().expect("slowest_n") as usize;
    let mocked = &got["suites"]["mocked"];
    assert_eq!(mocked["count"], 10, "{got}");
    let slowest = mocked["slowest"].as_array().expect("mocked slowest");
    assert_eq!(slowest.len(), n.min(10), "{got}");
    assert_eq!(slowest[0]["ms"], 27612, "{got}");
    assert_eq!(slowest[0]["result"], "failed");
    let name = slowest[0]["test"].as_str().expect("test name");
    assert!(
        name.starts_with("tests/mocked/it-department-map-stations.mocked.spec.ts:375 › draws its "),
        "{name}"
    );
    let ms: Vec<u64> = slowest
        .iter()
        .map(|e| e["ms"].as_u64().expect("ms"))
        .collect();
    assert!(
        ms.windows(2).all(|w| w[0] >= w[1]),
        "not slowest first: {ms:?}"
    );

    let unit = &got["suites"]["unit:web"];
    assert_eq!(
        unit["count"], 2,
        "a line whose time is not a number is not counted: {got}"
    );
    assert_eq!(unit["slowest"][0]["ms"], 30155, "{got}");
    assert_eq!(unit["slowest"][1]["test"], "./src/router.test.ts", "{got}");
}

#[test]
fn a_web_phase_that_recorded_nothing_says_so_rather_than_null() {
    let got = receipt_json("empty", "web_timings_begin");
    assert_eq!(got["gate_load"], "", "{got}");
    assert_eq!(got["suites"], serde_json::json!({}), "{got}");
}
