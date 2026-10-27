//! `infra/platform/workflows/protocol-retro.toml` carries the tier mix
//! as a STANDING reading (backlog 79fdc808, design 32f18167). One pin
//! file per kind file (see `platform_bundle.rs`), so a new pin touches
//! no shared line.
//!
//! The decision was that "is the core settling while work moves
//! outward" is answered where a decision is already being made, every
//! period, rather than argued — and protocol-retro is the platform's
//! weekly retro (the `department-retros-weekly` rule opens it beside
//! every department's). A reading the procedure merely MENTIONS is one
//! a busy week skips, so `collect` REQUIRES the field: the step cannot
//! complete without the reading, the same way `report` cannot complete
//! without `sign_off_context`.

use boss_jobs::car::{PROOF_ATTEMPT, PROOF_OWED};
use boss_jobs::corrections::CORRECTIONS_KEY;
use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn protocol_retro() -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == "protocol-retro")
        .expect("protocol-retro ships in the platform bundle")
}

#[test]
fn the_retro_collect_step_requires_the_tier_mix_reading() {
    let wf = protocol_retro();
    let collect = wf
        .steps
        .iter()
        .find(|s| s.title == "collect")
        .expect("protocol-retro has a collect step");
    let field = collect
        .fields
        .iter()
        .find(|f| f.name == "tier_mix")
        .expect("collect declares a tier_mix field");
    assert!(
        field.required,
        "tier_mix is REQUIRED at collect: a standing reading, not a mention"
    );
    let procedure = collect
        .metadata_defaults
        .get("procedure")
        .and_then(|v| v.as_str())
        .expect("collect carries a procedure");
    // The verb the reading comes from, and the two rules that make it a
    // reading rather than a target: coverage stated, no ratio to meet.
    for phrase in [
        "boss channels --tiers",
        "coverage",
        "NO TARGET RATIO",
        "OUTWARD",
    ] {
        assert!(
            procedure.contains(phrase),
            "collect's procedure names `{phrase}`"
        );
    }
}

/// Beside the core-changes count (bd93d2be) and the tier mix sits the
/// DIRECTION the work on the core should take (backlog 4332f3cf,
/// design 93d2bddb): a change that REMOVES a defect class buys one core
/// change and then none, while one that ENUMERATES another case buys a
/// core change per incident forever. It is prose by decision, not a
/// metric — nothing in the record marks a change as either — so the pin
/// holds the direction, the refusal to invent a number, and the three
/// readings named together in the one place a retro reads them.
#[test]
fn the_retro_collect_step_names_class_removal_as_the_direction_beside_the_readings() {
    let wf = protocol_retro();
    let collect = wf
        .steps
        .iter()
        .find(|s| s.title == "collect")
        .expect("protocol-retro has a collect step");
    let procedure = collect
        .metadata_defaults
        .get("procedure")
        .and_then(|v| v.as_str())
        .expect("collect carries a procedure");
    for phrase in [
        "4332f3cf",
        "bd93d2be",
        "REMOVED",
        "ENUMERATED",
        "THE DIRECTION WE ARE TRYING TO CAUSE is removal",
        "PROSE, NOT A SECOND NUMBER",
        "the tier mix asks whether work is moving outward",
    ] {
        assert!(
            procedure.contains(phrase),
            "collect's procedure names `{phrase}`"
        );
    }
}

/// The other half of the reading (backlog bd93d2be): the ABSOLUTE
/// number of core changes per day, the crest signal David chose on
/// 2026-09-19 — beside the tier mix, required at collect for the same
/// reason, and read in the direction he named with no target.
#[test]
fn the_retro_collect_step_requires_the_core_changes_reading() {
    let wf = protocol_retro();
    let collect = wf
        .steps
        .iter()
        .find(|s| s.title == "collect")
        .expect("protocol-retro has a collect step");
    let field = collect
        .fields
        .iter()
        .find(|f| f.name == "core_changes")
        .expect("collect declares a core_changes field");
    assert!(
        field.required,
        "core_changes is REQUIRED at collect: a standing reading, not a mention"
    );
    let procedure = collect
        .metadata_defaults
        .get("procedure")
        .and_then(|v| v.as_str())
        .expect("collect carries a procedure");
    for phrase in [
        "boss channels --core-changes",
        "git fetch origin",
        "ABSOLUTE",
        "NO TARGET AND NO THRESHOLD",
        "DOWN",
    ] {
        assert!(
            procedure.contains(phrase),
            "collect's procedure names `{phrase}`"
        );
    }
}

/// The gate-run verdict split is read off a LIST, and since backlog
/// ea80b5fd a list row's steps are slim unless the read asks
/// `full=true`. The verdict and receipt are `record-verdict` step
/// METADATA (gate-run.toml), and the job's own `outcome` is only
/// completed/failed, which cannot tell `lost` from `failed` — so the
/// procedure counts the terminal step each packet completed, which a
/// slim row keeps, and names the flag for the rest.
#[test]
fn the_retro_reads_the_gate_verdict_split_from_what_a_slim_row_keeps() {
    let wf = protocol_retro();
    let collect = wf
        .steps
        .iter()
        .find(|s| s.title == "collect")
        .expect("protocol-retro has a collect step");
    let procedure = collect
        .metadata_defaults
        .get("procedure")
        .and_then(|v| v.as_str())
        .expect("collect carries a procedure");
    for phrase in [
        "GET /api/jobs?kind=gate-run",
        "green / failed / lost / refused",
        "a slim list row keeps",
        "&full=true",
    ] {
        assert!(
            procedure.contains(phrase),
            "collect's procedure names `{phrase}`"
        );
    }
}

/// THE PLATFORM RETRO COUNTS ERRORS AND CAVEATS UNDER THE KEYS THEIR
/// DOORS WRITE (backlog 78a42b2a). Collect named `correction`,
/// `proof_note` and `awaiting_proof` as prose; measured 2026-10-21 on
/// the live jobs API, all time: 1 hand-typed packet carried
/// `proof_note`, none `awaiting_proof` (the shed's derived state, never
/// stored), while 74 carried `corrections` and 364 `proof_attempt`.
/// Department-retro's pin holds the same three keys for its collect;
/// this holds the platform's, as reads, to the writers' constants.
#[test]
fn the_retro_counts_errors_and_caveats_under_the_keys_their_doors_write() {
    let wf = protocol_retro();
    let collect = wf
        .steps
        .iter()
        .find(|s| s.title == "collect")
        .expect("protocol-retro has a collect step");
    let procedure = collect
        .metadata_defaults
        .get("procedure")
        .and_then(|v| v.as_str())
        .expect("collect carries a procedure");
    let door = "metadata_has=";
    let keys: Vec<&str> = procedure
        .match_indices(door)
        .map(|(at, _)| {
            let rest = &procedure[at + door.len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            &rest[..end]
        })
        .collect();
    let written = [CORRECTIONS_KEY, PROOF_ATTEMPT, PROOF_OWED];
    for key in written {
        assert!(
            keys.contains(&key),
            "collect counts `{door}{key}`; it asks for {keys:?}"
        );
    }
    for stray in ["correction", "proof_note", "awaiting_proof"] {
        assert!(
            !procedure.contains(&format!("`{stray}`")) && !keys.contains(&stray),
            "collect still counts `{stray}`, which no door writes"
        );
    }

    // THE PROOF READS COUNT DEPARTURES, AND SAY WHAT THEY COUNT (review
    // b4bae4e7). `closed_within` alone also returns every live packet,
    // and a live car's `proof_attempt` is usually a wait that has not
    // happened yet; measured 2026-10-21, IT, 7 days: 69 without
    // `terminal=true`, 65 with it. Department-retro's pin holds the
    // same for its collect, and says what was measured.
    for key in [PROOF_ATTEMPT, PROOF_OWED] {
        let ask = format!("{door}{key}");
        let reads: Vec<&str> = procedure
            .match_indices(&ask)
            .filter_map(|(at, _)| {
                let start = procedure[..at].rfind("/api/jobs?")?;
                Some(&procedure[start..at + ask.len()])
            })
            .collect();
        assert!(!reads.is_empty(), "collect reads `{ask}`");
        for read in reads {
            assert!(
                read.contains("terminal=true"),
                "`{read}` counts live packets too; only terminal=true keeps departures"
            );
        }
    }
    assert!(
        procedure.contains("did not settle on its first run"),
        "collect says what proof_attempt records: a run that settled nothing"
    );
    for split in ["proof_attempt.exit", "not_yet"] {
        assert!(
            procedure.contains(split),
            "collect tells the analyst to split the rows by `{split}`"
        );
    }
    assert!(
        !procedure.contains("did not hold the first time"),
        "collect still reads a not-yet wait as a failed proof"
    );
    // Review32328d33: an expectation miss exits0; a missing-tool run
    // carries its real nonzero exit. Judge flags before exit codes.
    let clauses = [
        "refused or environment",
        "otherwise not_yet true or exit 75",
        "otherwise unrunnable true",
        "every remaining record is a failure",
        "including exit 0 when proof_expect was absent",
    ];
    let positions: Vec<_> = clauses
        .iter()
        .map(|clause| procedure.find(clause))
        .collect();
    let missing: Vec<_> = clauses
        .iter()
        .zip(&positions)
        .filter_map(|(clause, position)| position.is_none().then_some(*clause))
        .collect();
    assert!(
        missing.is_empty(),
        "collect omits proof categories: {missing:?}"
    );
    let positions: Vec<_> = positions.into_iter().flatten().collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "collect must judge refusal, wait, unrunnable, then failure in order"
    );
    assert!(
        !procedure.contains("only another non-zero exit is a failure"),
        "exit0 expectation misses are failures; missing-tool exits are not verdicts"
    );
}
