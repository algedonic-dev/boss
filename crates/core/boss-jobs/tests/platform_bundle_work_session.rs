//! `infra/platform/workflows/work-session.toml` keeps its decided shape
//! (design 511fa7d4, decided 2026-09-18; car 2b of c87fb59b, backlog
//! da925366). One pin file per kind file (see `platform_bundle.rs`), so
//! a new protocol touches no shared line.
//!
//! WHAT THIS PINS, AND WHY IT EXISTS
//! ---------------------------------
//! An operator's session is a packet: opened by the SessionStart hook,
//! kept alive by a heartbeat the UserPromptSubmit hook writes onto the
//! packet (`last_active_at`, `prompt_count` — never the prompt text),
//! ended by the SessionEnd hook or, when the session goes silent past
//! the bound, by a clock rule. The runs the session dispatched link back
//! to it by `metadata.session` on each agent-run, which is how the crew
//! board reads a session's runs. The one fact that lives twice — the
//! silence bound the clock rule declares against the `duration_hours`
//! this file authors — is held equal at 2x here, the same pin
//! `platform_bundle_agent_run.rs` holds for the run's own died rule.

use boss_jobs::registry::{StepSpec, WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

fn step<'a>(spec: &'a WorkflowSpec, title: &str) -> &'a StepSpec {
    spec.steps
        .iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("work-session has a `{title}` step"))
}

#[test]
fn the_session_has_the_decided_steps_in_order() {
    let session = bundled("work-session");
    let titles: Vec<&str> = session.steps.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, vec!["opened", "active", "ended"]);
    assert_eq!(step(&session, "opened").kind, "trigger");
    assert_eq!(step(&session, "opened").ready_when, "true");
    assert_eq!(step(&session, "active").ready_when, "steps.opened.done");
    assert_eq!(step(&session, "ended").ready_when, "steps.active.done");
    assert_eq!(
        step(&session, "ended")
            .terminal
            .as_ref()
            .map(|t| t.outcome.as_str()),
        Some("ended")
    );
}

/// `active` says HOW the session ended — by the hook (`clean`) or by
/// the clock (`silent`) — and cannot close without saying.
#[test]
fn active_says_how_the_session_ended() {
    let session = bundled("work-session");
    let field = step(&session, "active")
        .fields
        .iter()
        .find(|f| f.name == "ended")
        .expect("active declares `ended`");
    assert!(field.required);
    assert_eq!(field.field_type, "clean|silent");
}

/// The heartbeat's keys and the session's identity are declared on the
/// schema; only what the SessionStart hook writes is required, because
/// the heartbeat and the end arrive later, from other hooks.
#[test]
fn the_schema_requires_the_session_start_keys_only() {
    let session = bundled("work-session");
    let required: Vec<&str> = session.metadata_schema["required"]
        .as_array()
        .expect("required is a list")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(required, vec!["actor", "host", "started_at"]);
    for later in [
        "cwd",
        "session_id",
        "source",
        "last_active_at",
        "prompt_count",
        "untracked_runs",
        "ended_at",
        "end_reason",
    ] {
        assert!(
            session.metadata_schema["properties"].get(later).is_some(),
            "{later} is declared"
        );
    }
}

/// THE FACT THAT LIVES TWICE. The clock rule that ends a silent session
/// declares its bound as a rule arg, and the protocol authors the
/// duration here; held equal at 2x, the design's "silent past 6h".
/// The rule must also read the heartbeat: a session that is prompting
/// every hour has moved, even though no step completed, so the rule
/// names `last_active_at` as the movement it measures from.
#[test]
fn the_silence_bound_is_twice_the_active_steps_duration_and_reads_the_heartbeat() {
    let session = bundled("work-session");
    let duration = step(&session, "active")
        .duration_hours
        .expect("active authors duration_hours");
    let rule_file =
        boss_testing::repo_root().join("infra/dispatcher/rules/work-session-ends-when-silent.toml");
    let rule: toml::Value = toml::from_str(
        &std::fs::read_to_string(&rule_file).expect("the silent clock rule is authored"),
    )
    .expect("the rule file parses");
    let action = &rule["rule"][0]["do"][0];
    assert_eq!(action["handler"].as_str(), Some("jobs.age_out_step"));
    assert_eq!(action["args"]["kind"].as_str(), Some("\"work-session\""));
    assert_eq!(action["args"]["step"].as_str(), Some("\"active\""));
    assert_eq!(
        action["args"]["since_key"].as_str(),
        Some("\"last_active_at\""),
        "the heartbeat is movement"
    );
    let hours: f64 = action["args"]["hours"]
        .as_str()
        .expect("hours is an expression string")
        .trim_matches('"')
        .parse()
        .expect("hours is a number");
    assert_eq!(hours, 2.0 * duration, "the bound is 2x the step's duration");
    assert_eq!(hours, 6.0, "the design's own number");
    // The arg is an expression: a quoted string whose quotes are escaped.
    let done: serde_json::Value = serde_json::from_str(
        &action["args"]["done_metadata"]
            .as_str()
            .expect("done_metadata is an expression string")
            .trim_matches('"')
            .replace("\\\"", "\""),
    )
    .expect("done_metadata is JSON");
    assert_eq!(done["ended"], "silent");
    assert_eq!(
        rule["rule"][0]["schedule"]["cadence"].as_str(),
        Some("hourly"),
        "jobs.age_out_step reads the tick's `_at`, which only a sub-day cadence carries"
    );
}

/// THE WORKING WINDOW IS PACKET DATA (backlog 37e3295c). On 2026-10-18
/// and again on 2026-10-19 a factory session was told a number of hours
/// in chat, honoured it literally, and went idle with urgent
/// nominations ready — 80 minutes lost the first time, 48 the second —
/// because the window, its end and who could extend it lived only in
/// two transcripts. The protocol now declares it: one `window` object,
/// written by `boss session window`, whose `state` says open-ended, an
/// `until` instant, or closed — and a close carries its handoff, so the
/// other session reads why and when from the packet, not from a chat.
#[test]
fn the_schema_declares_the_working_window_and_its_close() {
    let session = bundled("work-session");
    let window = &session.metadata_schema["properties"]["window"];
    assert_eq!(window["type"], "object", "{window}");
    let states: Vec<&str> = window["properties"]["state"]["enum"]
        .as_array()
        .expect("state is an enum")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(states, vec!["open", "until", "closed"]);
    for key in [
        "until",
        "set_by",
        "set_at",
        "why",
        "closed_at",
        "closed_by",
        "handoff",
    ] {
        assert!(
            window["properties"].get(key).is_some(),
            "window declares {key}"
        );
    }
    assert_eq!(
        window["required"],
        serde_json::json!(["state"]),
        "a window always says which of the three it is"
    );
    // Not required on the packet: the SessionStart hook files it before
    // anyone has set a window, and an absent window reads open-ended.
    let required = session.metadata_schema["required"].to_string();
    assert!(!required.contains("window"), "{required}");
}
