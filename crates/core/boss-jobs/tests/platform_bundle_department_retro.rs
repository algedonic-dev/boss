//! `infra/platform/workflows/department-retro.toml` keeps its decided
//! shape (design 3613f0af, backlog 1dffde5d). One pin file per kind
//! file (see `platform_bundle.rs`), so a new protocol touches no shared
//! line.
//!
//! The decision was "the SAME steps as protocol-retro, parameterised by
//! department", so the pin is written as a COMPARISON against the
//! protocol-retro row rather than a second copy of the step list: the
//! two kinds' step titles, kinds, gates and terminals are held equal,
//! and a reshape of either one that the other does not follow fails
//! here by step name (CLAUDE.md §9a — a fact that lives twice gets an
//! equality test).

use boss_jobs::car::{PROOF_ATTEMPT, PROOF_OWED};
use boss_jobs::corrections::CORRECTIONS_KEY;
use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

/// The collect step's procedure text.
fn collect_procedure() -> String {
    bundled("department-retro")
        .steps
        .iter()
        .find(|s| s.title == "collect")
        .expect("a collect step")
        .metadata_defaults["procedure"]
        .as_str()
        .expect("collect carries a procedure")
        .to_string()
}

/// THE RETRO COUNTS CORRECTIONS UNDER THE KEY THE DOOR WRITES (backlog
/// 4fd40048). One fact, one spelling (CLAUDE.md §9a): the key
/// `boss correct` appends to is [`CORRECTIONS_KEY`], and the procedure
/// that counts them is held to that constant, not to a word typed
/// beside it. Measured 2026-10-19 on the live jobs API, IT, the week
/// closed: `metadata_has=correction` answered 0, `metadata_has=corrections`
/// 26, a key nothing carries 0 — every department retro run on v8 and
/// v9 of this row read its week's corrections as none.
#[test]
fn the_collect_procedure_counts_corrections_under_the_key_boss_correct_writes() {
    let procedure = collect_procedure();
    let door = "metadata_has=";
    let keys = metadata_has_keys(&procedure);
    assert!(
        keys.contains(&CORRECTIONS_KEY),
        "collect counts `{door}{CORRECTIONS_KEY}`, the reserved list `boss correct` \
         appends to; it asks for {keys:?}"
    );
    let strays: Vec<&&str> = keys
        .iter()
        .filter(|k| k.starts_with("correction") && **k != CORRECTIONS_KEY)
        .collect();
    assert!(
        strays.is_empty(),
        "collect counts corrections under {strays:?}, which `boss correct` never writes"
    );
}

/// Every key the procedure asks the listing for, as `metadata_has=<key>`.
fn metadata_has_keys(procedure: &str) -> Vec<&str> {
    let door = "metadata_has=";
    procedure
        .match_indices(door)
        .map(|(at, _)| {
            let rest = &procedure[at + door.len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            &rest[..end]
        })
        .collect()
}

/// THE RETRO COUNTS PROOF CAVEATS UNDER THE KEYS THE PROOF DOORS WRITE
/// (backlog 78a42b2a, the second half of 4fd40048's bullet). Collect
/// asked for `proof_note` and `awaiting_proof`; no door writes either.
/// Measured 2026-10-21 on the live jobs API (`metadata_has=<key>`,
/// all time / IT departed within 7 days): `proof_note` 1 / 0, a sentence
/// typed by hand on one 2026-09-17 car; `awaiting_proof` 0 / 0 — it is
/// the shed's DERIVED state (`regions::shed`), computed from steps and
/// never stored; `proof_attempt` 364 / 65, the record every probe door
/// leaves when a run settles nothing, kept after a later proof (358
/// closed cars with `proven` completed carry one) — 65 closed, plus 4
/// live cars a read without `terminal=true` also counts (69; see the
/// pin below); `proof_owed` 0 / 0,
/// the marker a car that lands owing its proof carries; a key nothing
/// carries 0 / 0. So collect counts [`PROOF_ATTEMPT`] and
/// [`PROOF_OWED`], held to the constants their writers use, and asks
/// for no other proof key.
#[test]
fn the_collect_procedure_counts_proof_caveats_under_the_keys_the_proof_doors_write() {
    let procedure = collect_procedure();
    let keys = metadata_has_keys(&procedure);
    for written in [PROOF_ATTEMPT, PROOF_OWED] {
        assert!(
            keys.contains(&written),
            "collect counts `metadata_has={written}`, a key the proof doors write; \
             it asks for {keys:?}"
        );
    }
    let unwritten: Vec<&&str> = keys
        .iter()
        .filter(|k| k.contains("proof") && ![PROOF_ATTEMPT, PROOF_OWED].contains(*k))
        .collect();
    assert!(
        unwritten.is_empty(),
        "collect counts proof caveats under {unwritten:?}, which no proof door writes"
    );
}

/// The listing URL a `metadata_has=<key>` read sits in: from the
/// nearest `/api/jobs?` before it up to the key. A key spelled outside
/// any listing read has none.
fn reads_asking_for<'a>(procedure: &'a str, key: &str) -> Vec<&'a str> {
    let ask = format!("metadata_has={key}");
    procedure
        .match_indices(&ask)
        .filter_map(|(at, _)| {
            let start = procedure[..at].rfind("/api/jobs?")?;
            Some(&procedure[start..at + ask.len()])
        })
        .collect()
}

/// THE PROOF CAVEATS ARE COUNTED OVER DEPARTURES, AND READ AS WHAT THEY
/// RECORD (backlog 78a42b2a, review b4bae4e7). `closed_within` alone
/// returns every LIVE packet plus the window's closed ones (http/jobs.rs,
/// `ListJobsQuery::closed_within`); only `terminal=true` keeps departures.
/// Measured 2026-10-21 ~19:06Z, IT, 7 days, `metadata_has=proof_attempt`:
/// 69 without `terminal=true`, 65 with it, 4 with `terminal=false` —
/// three of those four live cars held a not-yet record whose own text
/// reads "Not a verdict against the change", so a read without it counts
/// proofs that have not happened. And of the 65 closed, 38 held a
/// not-yet wait and 27 a failure, while the dispatcher writes its
/// refusals under the same key: so the procedure says the record means
/// the proof did not SETTLE on its first run, and tells the analyst to
/// split the rows by `exit` / `not_yet` before calling any a failure.
#[test]
fn the_proof_caveats_are_counted_over_departures_and_split_before_judged() {
    let procedure = collect_procedure();
    for key in [PROOF_ATTEMPT, PROOF_OWED] {
        let reads = reads_asking_for(&procedure, key);
        assert!(!reads.is_empty(), "collect reads `metadata_has={key}`");
        for read in reads {
            assert!(
                read.contains("terminal=true"),
                "`{read}` counts live packets too: closed_within alone keeps every \
                 open one, and only terminal=true keeps departures"
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

/// The agent-runs doors read `since` as a timestamp: a bare
/// `YYYY-MM-DD` is a 400 ("since: premature end of input", measured
/// 2026-10-19), and <window start> is defined as that bare day. So
/// every agent-runs `since=` placeholder says RFC 3339 (backlog
/// 4fd40048's second note).
#[test]
fn every_agent_runs_since_in_collect_is_rfc_3339() {
    let procedure = collect_procedure();
    let mut read = 0usize;
    for (at, _) in procedure.match_indices("/api/agent-runs") {
        let rest = &procedure[at..];
        let Some(q) = rest.find("since=<") else {
            continue;
        };
        // Only this door's own query: stop at the first space.
        if rest[..q].contains(' ') {
            continue;
        }
        let placeholder = &rest[q..rest[q..].find('>').map_or(rest.len(), |e| q + e)];
        read += 1;
        assert!(
            placeholder.contains("RFC 3339"),
            "an agent-runs read takes `{placeholder}>`, which the door refuses with a 400"
        );
    }
    assert!(read >= 2, "collect reads the profiles AND the run listing");
}

#[test]
fn the_it_retro_reads_the_whole_simplification_window_and_its_coverage() {
    let wf = bundled("department-retro");
    let collect = wf.steps.iter().find(|s| s.title == "collect").unwrap();
    let procedure = collect.metadata_defaults["procedure"].as_str().unwrap();
    for contract in [
        "retro-simplification.py",
        "--collect",
        "--ref",
        "--since",
        "--until",
        "median_files",
        "median_adds",
        "median_removes",
        "removes_adds_ratio",
        "core_share",
        "cars_landed",
        "coverage",
        "trend",
        "UNKNOWN",
    ] {
        assert!(procedure.contains(contract), "IT collect omits {contract}");
    }
    assert!(
        collect
            .fields
            .iter()
            .any(|f| f.name == "simplification" && !f.required)
    );
}

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

/// The steps a department retro walks are protocol-retro's steps —
/// same titles, same kinds, same `ready_when` gates, same two
/// terminals. Only the words on them (procedures, fields) speak of a
/// department rather than the pipeline.
#[test]
fn a_department_retro_walks_the_same_steps_as_the_protocol_retro() {
    let dept = bundled("department-retro");
    let proto = bundled("protocol-retro");
    assert_eq!(
        dept.subject_kinds, proto.subject_kinds,
        "the Subject is a custom id"
    );

    let shape = |w: &WorkflowSpec| -> Vec<(String, String, String, Option<String>)> {
        w.steps
            .iter()
            .map(|s| {
                (
                    s.title.clone(),
                    s.kind.clone(),
                    s.ready_when.clone(),
                    s.terminal.as_ref().map(|t| t.outcome.clone()),
                )
            })
            .collect()
    };
    assert_eq!(
        shape(&dept),
        shape(&proto),
        "department-retro's steps (title, kind, ready_when, terminal) are protocol-retro's"
    );
    assert_eq!(
        dept.steps.len(),
        8,
        "opened / collect / analyze / gaps / report / review / recorded / cancelled"
    );
}

/// The parameter: a packet of this kind names its department, and the
/// row says so in its admission contract.
#[test]
fn a_department_retro_requires_its_department() {
    let wf = bundled("department-retro");
    let required: Vec<&str> = wf.metadata_schema["required"]
        .as_array()
        .expect("metadata_schema.required is a list")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(required, vec!["department"]);
    assert_eq!(
        wf.metadata_schema["properties"]["department"]["type"], "string",
        "the department is a Class code"
    );
}

/// The constraint protocol-retro learned three times over: the reviewer
/// cannot be handed an empty packet, because `report` cannot complete
/// without `sign_off_context`, and `review` waits on `report`.
#[test]
fn the_sign_off_is_fed_by_the_report_step() {
    let wf = bundled("department-retro");
    let step = |title: &str| {
        wf.steps
            .iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("department-retro has no `{title}` step"))
    };
    let report = step("report");
    assert!(
        report
            .fields
            .iter()
            .any(|f| f.name == "sign_off_context" && f.required),
        "report requires sign_off_context; got {:?}",
        report.fields
    );
    let review = step("review");
    assert_eq!(review.kind, "sign-off");
    assert_eq!(review.ready_when, "steps.report.done");
    assert_eq!(review.authority_role.as_deref(), Some("platform-admin"));
    assert!(
        review
            .fields
            .iter()
            .any(|f| f.name == "decision" && f.required),
        "the sign-off records its decision"
    );

    // The gaps step FILES work with counts, and its output fields say so.
    let gaps = step("gaps");
    for name in ["repeated_manual_work", "protocol_gaps", "protocol_updates"] {
        assert!(
            gaps.fields.iter().any(|f| f.name == name && f.required),
            "gaps requires `{name}`"
        );
    }
    // The collect procedure reads the department's own doors, not IT's.
    let collect = step("collect");
    let procedure = collect.metadata_defaults["procedure"]
        .as_str()
        .expect("collect carries a procedure");
    for door in [
        "/api/departments/{department}/readiness",
        "/api/jobs?department={department}",
        "terminal-report",
        "/api/sensors",
    ] {
        assert!(procedure.contains(door), "collect names `{door}`");
    }
}

/// THE IT RETRO READS THE AGENT WORK PROFILE (backlog 2f23f4c6). The
/// 2026-09-24 builder analysis — where 96 runs' tool time and context
/// went — was an afternoon's hand work that nothing repeated. The meter
/// now records a profile per run and `GET /api/agent-runs/profiles`
/// answers the week's rollup; this pins that the IT retro reads it in
/// `collect`, ranks the four readings the packet named in `analyze`
/// (by the rollup's own key names, so a reader can find each one in
/// the answer), records the ranking in a field of its own, and that
/// `gaps` files the top one. Enhancing the existing retro rather than
/// adding a protocol was the packet's instruction.
#[test]
fn the_it_retro_collects_ranks_and_files_the_agent_work_profile() {
    let wf = bundled("department-retro");
    let step = |title: &str| {
        wf.steps
            .iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("department-retro has no `{title}` step"))
    };
    let procedure = |title: &str| {
        step(title).metadata_defaults["procedure"]
            .as_str()
            .unwrap_or_else(|| panic!("`{title}` carries a procedure"))
            .to_string()
    };

    let collect = procedure("collect");
    assert!(
        collect.contains("/api/agent-runs/profiles?since="),
        "collect reads the week's work profiles"
    );
    assert!(
        collect.contains("department is `it`"),
        "the profile is IT's reading: agents are the IT department's CPUs"
    );

    let analyze = procedure("analyze");
    for key in [
        "largest_time_share",
        "largest_context_share",
        "top_files_read",
        "runs_that_edited_nothing",
    ] {
        assert!(analyze.contains(key), "analyze ranks `{key}`");
    }
    assert!(
        step("analyze")
            .fields
            .iter()
            .any(|f| f.name == "work_profile" && !f.required),
        "the ranking has a field of its own, optional because only IT fills it"
    );

    let gaps = procedure("gaps");
    assert!(
        gaps.contains("work_profile"),
        "gaps reads the ranking analyze recorded"
    );
    assert!(
        gaps.contains("FILE THE TOP ONE"),
        "gaps files the top of the ranking"
    );
}

/// THE COLLECT PROCEDURE IS TRUE, AND BOUNDED, FOR A DEPARTMENT OF MANY
/// KINDS (backlog a22311a1). Once 61 platform workflows declare `it`,
/// the v6 procedure was false and too costly for the IT retro that
/// opens every Monday: it said ops-requests belong to no department
/// (`ops-request` is one of the 61), asked for a terminal-report for
/// EACH kind the readiness read named (61 reads), and paged every
/// packet closed in the window (~8,600 a week) — inside one $2
/// analyst run. Collect now takes its per-kind counts from ONE
/// readiness read with a window, reads totals rather than pages, and
/// samples a terminal report or a page only where a number surprises.
#[test]
fn the_collect_procedure_reads_counts_once_and_samples_only_surprises() {
    let wf = bundled("department-retro");
    let collect = wf
        .steps
        .iter()
        .find(|s| s.title == "collect")
        .expect("a collect step");
    let procedure = collect.metadata_defaults["procedure"]
        .as_str()
        .expect("collect carries a procedure");

    // One read answers every kind's counts for the window.
    assert!(
        procedure.contains("/api/departments/{department}/readiness?since="),
        "collect asks readiness for the window's per-kind counts"
    );
    // The departures and the live are counted apart, as totals.
    assert!(
        procedure.contains("terminal=true") && procedure.contains("terminal=false"),
        "collect reads the departed and the live as two totals"
    );
    assert!(
        procedure.contains("limit=1"),
        "a total is read off a one-row page, not by paging"
    );

    // The three false or unbounded instructions are gone.
    for gone in [
        "terminal-report for each kind",
        "page with offset= before counting",
        "NOT attributable to a department",
        "status=open&limit=500",
        "closed_within=<days>&limit=500",
    ] {
        assert!(
            !procedure.contains(gone),
            "collect still says `{gone}`: {procedure}"
        );
    }
    // Ops-requests are counted where their workflow row puts them —
    // by the readiness read, never by a read of their own.
    assert!(
        procedure.contains("ops-request"),
        "collect says where ops-requests are counted"
    );
}
