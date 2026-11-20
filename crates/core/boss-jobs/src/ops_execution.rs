//! How an ops-request's verb ENDED, read from its execution record —
//! the one classifier (design b7bb1625, backlog 50a1578f; decided by
//! David 2026-11-17, docs/architecture-decisions.md "An ops-request
//! records whether its verb ran apart from how it ended").
//!
//! Ops-request `02035ff0` ran a replica move that failed after 1,204 s
//! with `exit_code` 1 and closed `answered` / `outcome_kind` completed:
//! the runner writes `answered` for every verb it INVOKED, whatever the
//! exit, and the row forks on that disposition alone. Nothing on the
//! packet looked failed. Relabelling it `refused` would be a second
//! lie — the refusal contract says nothing ran — so the decision keeps
//! the two facts apart: the disposition says whether the verb ran, and
//! the exit says how it went. This module is the reading of both
//! together, and the READERS landed on it first (car d35db666). The
//! held ops-request row then forks its terminals on the same record
//! (`answered`, `not-yet`, `failed`, `indeterminate`), its outcome_kind
//! pinned equal to [`Execution::outcome_kind`] through the engine; every
//! legacy pinned request — which still closes `answered` on a nonzero
//! exit — reads `failed` here from the record it already carries.
//!
//! PURE, over the `execute` step's metadata as the runner wrote it, and
//! nothing else: no second stored copy of the exit (§9a; 50fede8b
//! collapsed the last one). Five readings:
//!
//! - **refused** — the runner declined before invocation; nothing ran.
//!   Its `aborted` outcome and its reason are kept.
//! - **answered** — exit 0. Execution success, never proof of the
//!   requested effect: the runner's separate effect qualification
//!   (`effect`, `effect_unproven`, `effect_unread`; backlog fdbb447e)
//!   rides beside it and a surface shows it.
//! - **failed** — the verb ran and exited nonzero; the raw exit is kept.
//!   A timeout is claimed only on the wrapper's own kill line, never on
//!   124 alone, and no signal is ever claimed from 128+N alone — the
//!   runner records no signal evidence today.
//! - **indeterminate** — the record does not say how the verb ended: an
//!   answered verb with no exit, a malformed one, a disposition this
//!   reader does not know, or fields that contradict each other (a
//!   refusal carrying an exit). Its `outcome_kind` is failed. Missing
//!   evidence is NEVER read as "nothing ran", and never as success.
//!
//! Neither failed nor indeterminate licenses a rerun: this module
//! judges a record, it schedules nothing.

use serde_json::Value;

/// EX_TEMPFAIL, the estate's "not yet" (53f54b3f): a verb that cannot
/// answer YET exits 75 having measured and written nothing. It is a
/// nonzero exit, so the decided contract reads it [`Execution::Failed`];
/// the readers that already treat it as not-a-failure (the
/// answered-request judges, the probe shed) ask [`Execution::not_yet`]
/// rather than spelling the number again.
pub const NOT_YET_EXIT: &str = "75";

/// The line the runner appends when `timeout` killed the verb
/// (`infra/ops/ops-runner.sh`, "command killed at Ns timeout"). It is
/// the wrapper's evidence; an exit of 124 without it proves nothing.
pub const TIMEOUT_MARKER: &str = "[ops-runner: command killed at ";

/// What an exit 0 showed about the verb's declared effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// The read-back line the runner matched.
    Shown(String),
    /// Why the run did not show its declared effect — every reader
    /// fails it, whatever the exit.
    Unproven(String),
    /// The verb file's own reason it declares no read-back yet: exit 0
    /// is all the run proves.
    Unread(String),
    /// The record carries none of the three (an older runner, or a
    /// verb run before fdbb447e).
    Undeclared,
}

/// How one ops-request's verb ended, as its execute record says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Execution {
    /// Nothing ran: the runner declined before invocation.
    Refused { reason: Option<String> },
    /// Exit 0, with the effect qualification beside it.
    Answered { effect: Effect },
    /// The verb ran and exited nonzero. `exit` is the raw digits;
    /// `timed_out` only when the wrapper's kill line is in the output.
    Failed { exit: String, timed_out: bool },
    /// The record does not say how the verb ended. `raw` is the exit as
    /// recorded, when anything was; `cause` says what was wrong.
    Indeterminate {
        raw: Option<String>,
        cause: &'static str,
    },
}

impl Execution {
    /// The one word every surface prints for this reading.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Refused { .. } => "refused",
            Self::Answered { .. } => "answered",
            Self::Failed { .. } => "failed",
            Self::Indeterminate { .. } => "indeterminate",
        }
    }

    /// The `outcome_kind` the decided contract gives this reading: a
    /// refusal is `aborted`, an answer `completed`, a not-yet `not_yet`
    /// (decided on backlog 50a1578f, 2026-11-18), and both a failure
    /// and an unknown result `failed`. The ops-request row's terminals
    /// declare the same four, pinned equal through the engine by
    /// boss-jobs' the_terminal_a_request_closes_on_is_how_its_verb_ended.
    pub fn outcome_kind(&self) -> &'static str {
        match self {
            Self::Refused { .. } => "aborted",
            Self::Answered { .. } => "completed",
            e if e.not_yet() => "not_yet",
            Self::Failed { .. } | Self::Indeterminate { .. } => "failed",
        }
    }

    /// The exit as recorded, when one was.
    pub fn exit(&self) -> Option<&str> {
        match self {
            Self::Answered { .. } => Some("0"),
            Self::Failed { exit, .. } => Some(exit),
            Self::Indeterminate { raw, .. } => raw.as_deref(),
            Self::Refused { .. } => None,
        }
    }

    /// A failure whose exit is the estate's "not yet" ([`NOT_YET_EXIT`]).
    pub fn not_yet(&self) -> bool {
        matches!(self, Self::Failed { exit, .. } if exit == NOT_YET_EXIT)
    }

    /// Only a clean exit that did not fail its own effect check is a
    /// success; everything else — refused, failed, unknown, an exit 0
    /// whose effect was not shown — is not.
    pub fn succeeded(&self) -> bool {
        matches!(self, Self::Answered { effect } if !matches!(effect, Effect::Unproven(_)))
    }
}

/// The recorded exit, normalised: a JSON string or integer, `None` for
/// absent, null or empty. Anything else comes back as its JSON text so
/// an indeterminate reading keeps the raw value.
fn recorded_exit(md: &Value) -> Option<String> {
    match md.get("exit_code")? {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

fn text(md: &Value, key: &str) -> Option<String> {
    md.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

/// PURE: classify a COMPLETED execute step's metadata. A caller holding
/// an execute that is not completed has no answer to classify — the
/// runner lands its keys before the status (backlog 2aa2b19e).
pub fn classify(md: &Value) -> Execution {
    let raw = recorded_exit(md);
    match md.get("disposition").and_then(Value::as_str) {
        Some("refused") => match raw {
            // A refusal ran nothing, so it has no exit. One that carries
            // an exit contradicts itself, and a contradiction is read
            // closed: it is neither a clean refusal nor an answer.
            Some(raw) => Execution::Indeterminate {
                raw: Some(raw),
                cause: "a refusal that records an exit — the record contradicts itself",
            },
            None => Execution::Refused {
                reason: text(md, "reason").or_else(|| text(md, "output")),
            },
        },
        Some("answered") => {
            let Some(raw) = raw else {
                return Execution::Indeterminate {
                    raw: None,
                    cause: "the verb was invoked and no exit was recorded",
                };
            };
            if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
                return Execution::Indeterminate {
                    raw: Some(raw),
                    cause: "the recorded exit is not an exit status",
                };
            }
            if raw.bytes().all(|b| b == b'0') {
                let effect = match (
                    text(md, "effect_unproven"),
                    text(md, "effect"),
                    text(md, "effect_unread"),
                ) {
                    (Some(why), _, _) => Effect::Unproven(why),
                    (None, Some(line), _) => Effect::Shown(line),
                    (None, None, Some(why)) => Effect::Unread(why),
                    (None, None, None) => Effect::Undeclared,
                };
                return Execution::Answered { effect };
            }
            let timed_out = raw == "124"
                && md
                    .get("output")
                    .and_then(Value::as_str)
                    .is_some_and(|o| o.contains(TIMEOUT_MARKER));
            Execution::Failed {
                exit: raw,
                timed_out,
            }
        }
        // No disposition, or one this reader does not know: the record
        // does not say whether the verb ran, and an absence is never
        // turned into "nothing ran".
        _ => Execution::Indeterminate {
            raw,
            cause: "the execute record names no disposition this reader knows",
        },
    }
}

/// PURE: the reading of a packet as the jobs API returns it (steps
/// embedded, each with `spec_slug`, `status` and `metadata`): `None`
/// while its `execute` step is absent or not completed.
pub fn of_packet(packet: &Value) -> Option<Execution> {
    let execute = packet
        .get("steps")?
        .as_array()?
        .iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("execute"))?;
    if execute.get("status").and_then(Value::as_str) != Some("completed") {
        return None;
    }
    Some(classify(execute.get("metadata").unwrap_or(&Value::Null)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn answered(exit: Value) -> Value {
        json!({ "disposition": "answered", "exit_code": exit, "output": "ran\n" })
    }

    /// THE CASE THAT FILED THIS (ops-request 02035ff0, pinned v17): the
    /// move ran 1,207 s, failed its rebuild wait and exited 1, and the
    /// packet closed `answered`. Read from that record it is FAILED.
    #[test]
    fn the_legacy_answered_move_that_exited_one_reads_failed() {
        let md = json!({
            "disposition": "answered", "exit_code": "1", "duration_ms": 1207430,
            "runner_host": "forge",
            "output": "move: FAILED after 1204s — new replica r-d1d689a5 still rebuilding\n",
        });
        let e = classify(&md);
        assert_eq!(
            e,
            Execution::Failed {
                exit: "1".into(),
                timed_out: false
            }
        );
        assert_eq!(e.label(), "failed");
        assert_eq!(e.outcome_kind(), "failed");
        assert!(!e.succeeded());
    }

    #[test]
    fn a_refusal_ran_nothing_and_keeps_its_reason() {
        let e = classify(&json!({
            "disposition": "refused", "output": "outside the allowlist",
            "reason": "outside the allowlist", "exit_code": null,
        }));
        assert_eq!(
            e,
            Execution::Refused {
                reason: Some("outside the allowlist".into())
            }
        );
        assert_eq!(e.outcome_kind(), "aborted");
        assert_eq!(e.exit(), None);
        assert!(!e.succeeded());
    }

    /// A refusal says nothing ran, so an exit beside it contradicts the
    /// record; read closed, never as a clean refusal or an answer.
    #[test]
    fn a_refusal_that_records_an_exit_is_indeterminate() {
        let e = classify(&json!({"disposition": "refused", "exit_code": "1"}));
        assert!(matches!(e, Execution::Indeterminate { raw: Some(ref r), .. } if r == "1"));
        assert_eq!(e.outcome_kind(), "failed");
    }

    #[test]
    fn an_exit_zero_answers_and_keeps_its_effect_qualification() {
        assert_eq!(
            classify(&answered(json!("0"))),
            Execution::Answered {
                effect: Effect::Undeclared
            }
        );
        let mut shown = answered(json!("0"));
        shown["effect"] = json!("acts: OK — deleted 3");
        assert_eq!(
            classify(&shown),
            Execution::Answered {
                effect: Effect::Shown("acts: OK — deleted 3".into())
            }
        );
        let mut unread = answered(json!("0"));
        unread["effect_unread"] = json!("prints no re-list");
        let e = classify(&unread);
        assert_eq!(
            e,
            Execution::Answered {
                effect: Effect::Unread("prints no re-list".into())
            }
        );
        assert!(e.succeeded());
        assert_eq!(e.outcome_kind(), "completed");
        // Unproven wins over a matched line: the runner writes one or the
        // other, and a record that carries both is read against itself.
        let mut unproven = answered(json!("0"));
        unproven["effect"] = json!("acts: OK");
        unproven["effect_unproven"] = json!("no line matched");
        let e = classify(&unproven);
        assert_eq!(
            e,
            Execution::Answered {
                effect: Effect::Unproven("no line matched".into())
            }
        );
        assert_eq!(e.label(), "answered");
        assert!(!e.succeeded());
    }

    #[test]
    fn a_numeric_exit_reads_like_its_string() {
        assert_eq!(
            classify(&answered(json!(1))),
            classify(&answered(json!("1")))
        );
        assert!(classify(&answered(json!(0))).succeeded());
    }

    /// 124 is a timeout only on the wrapper's own kill line; 128+N never
    /// claims a signal, because the runner records no signal evidence.
    #[test]
    fn a_timeout_is_claimed_only_on_the_wrappers_evidence() {
        let mut killed = answered(json!("124"));
        killed["output"] = json!("reading df\n\n[ops-runner: command killed at 300s timeout]\n");
        assert_eq!(
            classify(&killed),
            Execution::Failed {
                exit: "124".into(),
                timed_out: true
            }
        );
        assert_eq!(
            classify(&answered(json!("124"))),
            Execution::Failed {
                exit: "124".into(),
                timed_out: false
            }
        );
        assert_eq!(
            classify(&answered(json!("137"))),
            Execution::Failed {
                exit: "137".into(),
                timed_out: false
            }
        );
    }

    /// An invoked verb with no trustworthy exit is UNKNOWN — failed in
    /// outcome, never success and never "nothing ran" — and keeps the
    /// raw value it recorded.
    #[test]
    fn an_answer_with_no_trustworthy_exit_is_indeterminate() {
        for missing in [json!(null), json!("")] {
            let e = classify(&answered(missing));
            assert!(
                matches!(e, Execution::Indeterminate { raw: None, .. }),
                "{e:?}"
            );
            assert_eq!(e.outcome_kind(), "failed");
            assert!(!e.succeeded());
        }
        let no_key = classify(&json!({"disposition": "answered", "output": "x"}));
        assert!(matches!(no_key, Execution::Indeterminate { raw: None, .. }));
        for (bad, raw) in [
            (json!("abc"), "abc"),
            (json!("-1"), "-1"),
            (json!(" 1"), " 1"),
            (json!(1.5), "1.5"),
            (json!(true), "true"),
        ] {
            let e = classify(&answered(bad));
            assert!(
                matches!(e, Execution::Indeterminate { raw: Some(ref r), .. } if r == raw),
                "{e:?}"
            );
        }
    }

    #[test]
    fn a_record_with_no_known_disposition_never_reads_as_nothing_ran() {
        for md in [
            json!({"exit_code": "1"}),
            json!({"disposition": "weird", "exit_code": "0"}),
            json!({}),
        ] {
            let e = classify(&md);
            assert!(matches!(e, Execution::Indeterminate { .. }), "{e:?}");
            assert_ne!(e.label(), "refused");
        }
    }

    /// A not-yet is a nonzero exit, so it reads failed, and its
    /// outcome is its own (decided on backlog 50a1578f, 2026-11-18): the
    /// row closes it `answered` with outcome_kind `not_yet` — never
    /// failed, never indeterminate, never rerun — and this is the one
    /// place that says so.
    #[test]
    fn a_not_yet_is_a_failure_that_says_so() {
        let e = classify(&answered(json!(NOT_YET_EXIT)));
        assert_eq!(e.label(), "failed");
        assert!(e.not_yet());
        assert_eq!(e.outcome_kind(), "not_yet");
        assert!(!e.succeeded());
        assert!(!classify(&answered(json!("1"))).not_yet());
        assert_eq!(classify(&answered(json!("1"))).outcome_kind(), "failed");
    }

    #[test]
    fn a_packet_reads_only_a_completed_execute() {
        let packet = |status: &str| {
            json!({"steps": [
                {"spec_slug": "filed", "status": "completed", "metadata": {}},
                {"spec_slug": "execute", "status": status,
                 "metadata": {"disposition": "answered", "exit_code": "1"}},
            ]})
        };
        assert_eq!(of_packet(&packet("active")), None);
        assert_eq!(
            of_packet(&packet("completed")).map(|e| e.label()),
            Some("failed")
        );
        assert_eq!(of_packet(&json!({"steps": []})), None);
    }
}
