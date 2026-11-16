//! `boss ops <host> <verb> [args…] [--wait]` — file an ops-request from
//! the terminal, validated against the allowlist the runner itself
//! enforces, and optionally wait for the host's answer.
//!
//! THE DOOR HAD NO HANDLE. The ops-request packet (729329c6) replaced
//! ssh: a host's runner polls the system of record for packets naming
//! it, executes one of the allowlisted verbs in infra/ops/verbs/
//! with pattern-checked args, and writes stdout/stderr/exit onto the
//! step — in under two minutes, every time it was tried. But filing one
//! was five hand steps: write `{host, verb, args}` to a scratch file,
//! `boss job file --kind ops-request --subject-id <host> --title … --metadata
//! file.json`, then poll the packet by hand for its `execute` step. Five
//! were filed that way on 2026-09-12 alone (3d6daea9), two the session
//! before. This verb is the handle.
//!
//! ONE ALLOWLIST, READ TWICE, NEVER COPIED. `infra/ops/verbs/<name>.json`
//! — one file per verb, the name being the file name (5086842d) — is
//! the authority the runner enforces on the host; this verb applies the
//! same rules, from the same files compiled into boss-jobs
//! (`boss_jobs::ops_verbs`, which the jobs API's file-the-request door
//! reads too — backlog 7aef2405), at the terminal — unknown
//! verb, a host the verb does not serve, a missing required arg, an arg
//! outside its pattern / one_of / max — so a bad call is refused before
//! a packet exists rather than as a `refused` step a minute later. The
//! runner's check still runs: this is the early copy of its verdict,
//! not a replacement for it. A binary that lags main carries an older
//! allowlist; `boss orient`'s freshness line says so.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

// THE RULES LIVE IN boss-jobs (backlog 7aef2405). The allowlist's filing
// rules were this file's until the jobs API needed them too, to file the
// write of a rendered plan from the website; one rule set is read by both
// doors, from `boss_jobs::ops_verbs`, which compiles the verb files in.
use boss_jobs::ops_verbs::Refusal;
pub(crate) use boss_jobs::ops_verbs::{Validated, request_metadata};

/// The allowlist this binary was built with — the directory
/// `infra/ops/verbs/`, compiled into boss-jobs, in the `{"verbs": {...}}`
/// shape the runner reads.
pub(crate) fn shipped_allowlist() -> Result<Value> {
    boss_jobs::ops_verbs::shipped_allowlist().map_err(anyhow::Error::msg)
}

/// The allowlist's rules applied to a call, as `ops-runner.sh` applies
/// them ([`boss_jobs::ops_verbs::validate`]), refused in this verb's
/// voice. The rule says what is wrong with the args; how to type them
/// instead is this door's to say (Codex review of car 72e7a790), so a
/// supplied plan hash is answered with the `boss ops` form that files,
/// word for word as main's 2267b4f6 gave it.
pub(crate) fn validate(
    verbs: &Value,
    host: &str,
    verb: &str,
    args: &[String],
) -> Result<Validated> {
    boss_jobs::ops_verbs::validate(verbs, host, verb, args).map_err(|r| match &r {
        Refusal::PlanHashSupplied {
            host, verb, params, ..
        } => anyhow!(
            "boss ops: {} File it as: boss ops {host} {verb}{}",
            r.reason_with("a hash given here"),
            params.iter().map(|n| format!(" <{n}>")).collect::<String>()
        ),
        Refusal::Rule(why) => anyhow!("boss ops: {why}"),
    })
}

/// The answer a closed ops-request carries on its `execute` step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answer {
    pub disposition: String,
    pub exit_code: Option<String>,
    pub output: String,
    pub runner_host: String,
    /// The runner's judgement of the verb's declared effect on an exit 0
    /// (backlog fdbb447e part 1): the read-back line it matched…
    pub effect: Option<String>,
    /// …or why the run did not show it…
    pub effect_unproven: Option<String>,
    /// …or the verb file's own reason it declares no read-back yet.
    pub effect_unread: Option<String>,
}

/// Read the `execute` step's record off a packet, or `None` while the
/// host has not answered.
pub(crate) fn answer_of(job: &Value) -> Option<Answer> {
    let step_of = |slug: &str| {
        job.get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
    };
    // A request the runner closed through its `refused` terminal while
    // `execute` still waited behind the approve step — a verb that
    // cannot carry an approval, or a plan verb that refused — answers
    // there, with the reason on that step.
    // So does one it closed through `nothing-to-do` (backlog b2f78bb9):
    // the plan it rendered named no change, so nothing ran and nobody was
    // asked, and the plan is the answer.
    let closed_terminal = |slug: &str, disposition: &str, text_key: &str| {
        let s = step_of(slug)?;
        if s.get("status").and_then(Value::as_str) != Some("completed") {
            return None;
        }
        let md = s.get("metadata")?;
        Some(Answer {
            disposition: disposition.to_string(),
            exit_code: None,
            output: md.get(text_key).and_then(Value::as_str)?.to_string(),
            runner_host: md
                .get("runner_host")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            effect: None,
            effect_unproven: None,
            effect_unread: None,
        })
    };
    // And one its independent review sent back (backlog 84e7c3e4, review
    // N4 of car e1f4fc2a): a refusal, closed on `not-released`. The engine
    // readies that terminal as the review completes, so the dispatcher
    // may close it before the runner does, with no reason on it; the
    // reviewer's findings on the review step are then the why.
    let not_released = || {
        closed_terminal("not-released", "refused", "reason").or_else(|| {
            step_of("not-released")
                .filter(|s| s.get("status").and_then(Value::as_str) == Some("completed"))?;
            Some(Answer {
                disposition: "refused".to_string(),
                exit_code: None,
                output: step_of("review")
                    .and_then(|s| s.get("metadata"))
                    .and_then(|m| m.get("findings"))
                    .and_then(Value::as_str)
                    .unwrap_or("its independent review asked for changes")
                    .to_string(),
                runner_host: String::new(),
                effect: None,
                effect_unproven: None,
                effect_unread: None,
            })
        })
    };
    let closed_refused = || {
        closed_terminal("refused", "refused", "reason")
            .or_else(|| closed_terminal("nothing-to-do", "nothing-to-do", "plan"))
            .or_else(not_released)
    };
    // Only a COMPLETED execute answers (the review of car 24eb9471). The
    // runner lands its keys through the merge door BEFORE the status
    // (backlog 2aa2b19e), so keys on an open execute are an answer the
    // server has not taken — a refused or lost status PUT — not one.
    let Some(md) = step_of("execute")
        .filter(|s| s.get("status").and_then(Value::as_str) == Some("completed"))
        .and_then(|s| s.get("metadata"))
    else {
        return closed_refused();
    };
    let Some(disposition) = md.get("disposition").and_then(Value::as_str) else {
        return closed_refused();
    };
    let disposition = disposition.to_string();
    let text = |key: &str| md.get(key).and_then(Value::as_str).map(str::to_string);
    Some(Answer {
        disposition,
        exit_code: md
            .get("exit_code")
            .and_then(Value::as_str)
            .map(str::to_string),
        output: md
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        runner_host: md
            .get("runner_host")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        effect: text("effect"),
        effect_unproven: text("effect_unproven"),
        effect_unread: text("effect_unread"),
    })
}

/// The one-line verdict `--wait` ends with, and whether it is a success.
///
/// An exit 0 is read WITH the effect the runner recorded beside it
/// (backlog fdbb447e part 1): one that did not show its declared effect
/// fails, loudly, naming what was looked for — the verb has already run,
/// so this refuses nothing; it declines to call the run a success. A
/// verb that declares no read-back yet (`effect_unread`) still succeeds
/// and says so in capitals, so exit 0 is never mistaken for more than it
/// proves.
pub(crate) fn verdict_line(id8: &str, v: &Validated, a: &Answer) -> (String, bool) {
    match (a.disposition.as_str(), a.exit_code.as_deref()) {
        ("answered", Some("0")) => {
            let head = format!(
                "boss ops: answered — {} on {} (packet {id8}, exit 0)",
                v.verb, a.runner_host
            );
            match (&a.effect, &a.effect_unproven, &a.effect_unread) {
                (_, Some(why), _) => (format!("{head}, but its EFFECT NOT SHOWN: {why}"), false),
                (Some(line), _, _) => (format!("{head}; effect shown: {line}"), true),
                (_, _, Some(why)) => (
                    format!(
                        "{head}; EFFECT UNREAD — this verb prints no read-back of what it changed, so exit 0 is all this run proves: {why}"
                    ),
                    true,
                ),
                _ => (head, true),
            }
        }
        ("answered", code) => (
            format!(
                "boss ops: answered — {} on {} (packet {id8}, exit {})",
                v.verb,
                a.runner_host,
                code.unwrap_or("?")
            ),
            false,
        ),
        // The plan named no change, so nothing ran and nobody was asked
        // (backlog b2f78bb9): the request's answer is that it needed none.
        ("nothing-to-do", _) => (
            format!(
                "boss ops: nothing to do — {} on {} (packet {id8}): the plan names no change, so nothing ran and no passkey was asked",
                v.verb, a.runner_host
            ),
            true,
        ),
        (d, _) => (
            format!("boss ops: {d} — {} on {} (packet {id8})", v.verb, v.host),
            false,
        ),
    }
}

pub async fn run(
    host: String,
    verb: String,
    args: Vec<String>,
    wait: bool,
    dry_run: bool,
) -> Result<()> {
    let verbs = shipped_allowlist()?;
    let v = validate(&verbs, &host, &verb, &args)?;
    if v.mutating {
        eprintln!(
            "boss ops: `{}` is MUTATING on {} — the host's runner applies the verb's own bounds; \
             this files the request, it does not widen them.",
            v.verb, v.host
        );
    }
    if v.requires_approval {
        eprintln!(
            "boss ops: `{}` requires a passkey approval — the runner on {} renders its plan onto \
             the packet's approve step, and runs it only after that step is signed with a \
             passkey (within ten minutes of the signature, once).",
            v.verb, v.host
        );
    }
    if v.requires_review {
        eprintln!(
            "boss ops: `{}` requires an independent review first — the runner renders the plan \
             onto the packet's review step for a tenant-reviewer run, and puts it in front of a \
             passkey only once that review releases those exact bytes.",
            v.verb
        );
    }
    let metadata = request_metadata(&v);
    if dry_run {
        println!(
            "boss ops: DRY RUN — would file ops-request \"{}\" (metadata {metadata})",
            v.title
        );
        return Ok(());
    }
    let http = crate::sor_client::machine_client()?;
    let owner = crate::identity::sign(&reqwest::Method::POST, "/api/jobs")?;
    // The one envelope the website's filing doors send too (backlog
    // 7aef2405), so a request filed here and one filed there are the same.
    let body = boss_jobs::ops_verbs::request_envelope(&v, &owner);
    let created = crate::sor_client::api(&http, reqwest::Method::POST, "/api/jobs", Some(body))
        .await?
        .context("the create returned no body")?;
    let id = created
        .get("id")
        .and_then(Value::as_str)
        .context("the create returned no id — refusing to call that filed")?
        .to_string();
    let id8 = &id[..8.min(id.len())];
    println!("boss ops: filed {id8} — \"{}\"", v.title);
    if !wait {
        println!(
            "boss ops: the runner on {} polls about once a minute; `boss job get {id8}` reads the answer",
            v.host
        );
        return Ok(());
    }
    // The packet is the record; poll it, as `boss gate --wait` does.
    // A host whose runner is down never answers: say so after a bound
    // rather than waiting forever on a packet nobody will touch.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15 * 60);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        let job = crate::sor_client::api(
            &http,
            reqwest::Method::GET,
            &format!("/api/jobs/{id}"),
            None,
        )
        .await?
        .context("the filed packet vanished")?;
        let job = job.get("data").unwrap_or(&job).clone();
        if let Some(a) = answer_of(&job) {
            let (line, ok) = verdict_line(id8, &v, &a);
            if !a.output.trim().is_empty() {
                println!("{}", a.output.trim_end());
            }
            println!("{line}");
            if ok {
                return Ok(());
            }
            bail!("{line}");
        }
        if std::time::Instant::now() > deadline {
            bail!(
                "boss ops: no answer from {} in 15 minutes — its runner polls every minute, so it \
                 is down or cannot reach the system of record. The packet {id8} stays open and \
                 visibly unanswered; `boss job get {id8}` when the host is back.",
                v.host
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // The allowlist's own rules are tested where they live,
    // boss_jobs::ops_verbs; these hold what this verb does with them.
    fn fixture() -> Value {
        json!({"verbs": {
            "journal-tail": {"about": "last N journal lines", "hosts": ["boss-gcp", "forge"],
                "argv": ["journalctl", "-u", "{1}", "-n", "{2}", "--no-pager"],
                "params": [
                    {"name": "unit", "pattern": "^[A-Za-z0-9@._][A-Za-z0-9@._-]*$"},
                    {"name": "lines", "pattern": "^[0-9]{1,3}$", "default": "100", "max": 200}]},
            "delete-orphan-object": {"about": "MUTATING — delete one undeclared object", "hosts": ["forge"],
                "argv": ["infra/forge/delete-orphan-object.sh", "{1}", "{2}"],
                "params": [
                    {"name": "object", "pattern": "^[A-Z][A-Za-z0-9]{0,62}/[a-z0-9-]+/[a-z0-9.-]+$"},
                    {"name": "mode", "one_of": ["--dry-run"], "optional": true}]},
            "uptime": {"about": "uptime", "hosts": ["boss-gcp", "forge"], "argv": ["uptime"], "params": []}
        }})
    }

    /// A request the runner closed through its `refused` terminal —
    /// execute still pending behind the approve step — answers there,
    /// so `--wait` ends on the reason instead of timing out.
    #[test]
    fn a_request_closed_refused_before_execute_answers_with_its_reason() {
        let v = validate(&fixture(), "forge", "uptime", &[]).unwrap();
        let job = json!({"steps": [
            {"spec_slug": "execute", "status": "skipped", "metadata": {"authority_role": "platform-admin"}},
            {"spec_slug": "refused", "status": "completed",
             "metadata": {"outcome_kind": "aborted", "runner_host": "forge",
                          "reason": "the plan verb plan-a-pod-reap refused (exit 78)"}}
        ]});
        let a = answer_of(&job).expect("the refused terminal is an answer");
        assert_eq!(a.disposition, "refused");
        assert!(a.output.contains("plan-a-pod-reap refused"), "{a:?}");
        let (line, ok) = verdict_line("abcd1234", &v, &a);
        assert!(!ok && line.starts_with("boss ops: refused"), "{line}");
        // A refused step not yet completed is no answer.
        let open = json!({"steps": [
            {"spec_slug": "execute", "status": "pending", "metadata": {}},
            {"spec_slug": "refused", "status": "pending", "metadata": {"outcome_kind": "aborted"}}
        ]});
        assert!(answer_of(&open).is_none());
    }

    /// A request its independent review sent back closes on
    /// `not-released` (backlog 84e7c3e4, review N4 of car e1f4fc2a) and
    /// answers there as a refusal: with the runner's reason when the
    /// runner closed it, else — the engine readies the terminal the moment
    /// the review completes, so the dispatcher may close it first, with no
    /// reason — with the reviewer's own findings.
    #[test]
    fn a_request_its_review_sent_back_answers_refused_with_why() {
        let v = validate(&fixture(), "forge", "uptime", &[]).unwrap();
        let review = json!({"spec_slug": "review", "status": "completed",
            "metadata": {"verdict": "changes", "findings": "the branch drops a ledger row"}});
        let job = json!({"steps": [
            review.clone(),
            {"spec_slug": "execute", "status": "skipped", "metadata": {}},
            {"spec_slug": "refused", "status": "skipped", "metadata": {"outcome_kind": "aborted"}},
            {"spec_slug": "not-released", "status": "completed",
             "metadata": {"outcome_kind": "aborted"}}
        ]});
        let a = answer_of(&job).expect("the not-released terminal is an answer");
        assert_eq!(a.disposition, "refused");
        assert!(a.output.contains("drops a ledger row"), "{a:?}");
        let (line, ok) = verdict_line("abcd1234", &v, &a);
        assert!(!ok && line.starts_with("boss ops: refused"), "{line}");
        let mut by_runner = job.clone();
        by_runner["steps"][3]["metadata"]["reason"] =
            json!("its review step records verdict \"changes\"");
        by_runner["steps"][3]["metadata"]["runner_host"] = json!("forge");
        let a = answer_of(&by_runner).unwrap();
        assert!(a.output.contains("verdict \"changes\""), "{a:?}");
        assert_eq!(a.runner_host, "forge");
        let open = json!({"steps": [
            review,
            {"spec_slug": "not-released", "status": "ready", "metadata": {}}
        ]});
        assert!(
            answer_of(&open).is_none(),
            "a terminal not yet completed is no answer"
        );
    }

    /// A request the runner closed through its `nothing-to-do` terminal
    /// (backlog b2f78bb9) answers there with the plan that named no
    /// change, and `--wait` ends on it as a success — nothing needed
    /// doing, and nobody was asked — instead of waiting fifteen minutes
    /// on an execute that will never run.
    #[test]
    fn a_request_closed_with_nothing_to_do_answers_with_its_plan() {
        let v = validate(&fixture(), "forge", "uptime", &[]).unwrap();
        let job = json!({"steps": [
            {"spec_slug": "execute", "status": "skipped", "metadata": {}},
            {"spec_slug": "refused", "status": "skipped", "metadata": {"outcome_kind": "aborted"}},
            {"spec_slug": "nothing-to-do", "status": "completed",
             "metadata": {"outcome_kind": "skipped", "runner_host": "forge",
                          "plan": "would vacuum the journal to 1G\n"}}
        ]});
        let a = answer_of(&job).expect("the nothing-to-do terminal is an answer");
        assert_eq!(a.disposition, "nothing-to-do");
        assert_eq!(a.output, "would vacuum the journal to 1G\n");
        let (line, ok) = verdict_line("abcd1234", &v, &a);
        assert!(ok && line.starts_with("boss ops: nothing to do"), "{line}");
        let open = json!({"steps": [
            {"spec_slug": "execute", "status": "pending", "metadata": {}},
            {"spec_slug": "nothing-to-do", "status": "ready", "metadata": {}}
        ]});
        assert!(
            answer_of(&open).is_none(),
            "a terminal not yet completed is no answer"
        );
    }

    /// `unit-list` takes one glob of unit-name characters and nothing a
    /// shell could read: `boss-*` is admitted, a space or a `;` is not,
    /// and the runner receives it as its own argv element.
    #[test]
    fn unit_list_admits_a_glob_and_refuses_a_shell() {
        let v = shipped_allowlist().expect("the verb files parse");
        let ok = validate(&v, "boss-gcp", "unit-list", &["boss-*".into()]).unwrap();
        assert_eq!(ok.args, vec!["boss-*".to_string()]);
        assert!(!ok.mutating);
        for bad in ["boss-* ; id", "boss-*;id", "$(id)", "a b"] {
            let e = validate(&v, "boss-gcp", "unit-list", &[bad.into()])
                .unwrap_err()
                .to_string();
            assert!(e.contains("does not match"), "{bad}: {e}");
        }
    }

    /// The answer is read off the packet's execute step and judged the
    /// way the runner wrote it: answered+0 is success; anything else is
    /// a non-zero exit for the caller.
    #[test]
    fn the_answer_is_judged_off_the_execute_step() {
        let v = validate(&fixture(), "forge", "uptime", &[]).unwrap();
        let job = json!({"steps": [
            {"spec_slug": "filed", "status": "completed", "metadata": {}},
            {"spec_slug": "execute", "status": "completed",
             "metadata": {"disposition": "answered", "exit_code": "0", "output": " 18:40:01 up 9 days\n", "runner_host": "forge"}}
        ]});
        let a = answer_of(&job).unwrap();
        let (line, ok) = verdict_line("abcd1234", &v, &a);
        assert!(
            ok && line.contains("exit 0") && line.contains("on forge"),
            "{line}"
        );
        let refused = json!({"steps": [{"spec_slug": "execute", "status": "completed",
            "metadata": {"disposition": "refused", "output": "verb x does not serve this host", "runner_host": "forge"}}]});
        let (line, ok) = verdict_line("abcd1234", &v, &answer_of(&refused).unwrap());
        assert!(!ok && line.starts_with("boss ops: refused"), "{line}");
        assert!(
            answer_of(
                &json!({"steps": [{"spec_slug": "execute", "status": "ready", "metadata": {}}]})
            )
            .is_none()
        );
    }

    /// AN EXIT 0 IS NOT AN EFFECT (backlog fdbb447e part 1). The runner
    /// judges a verb's declared `effect` on the run's output and records
    /// the verdict on the step; `--wait` reads it rather than re-deriving
    /// it. An exit 0 that did not show its declared effect is NOT a
    /// success — the verb ran, and nothing says it changed what it exists
    /// to change. A verb that declares no read-back yet still succeeds on
    /// exit 0, and says out loud that exit 0 is all the run proves.
    #[test]
    fn an_exit_0_is_judged_with_the_effect_the_runner_recorded() {
        let v = validate(&fixture(), "forge", "uptime", &[]).unwrap();
        let answered = |extra: Value| {
            let mut md = json!({"disposition": "answered", "exit_code": "0",
                                "output": "acts: deleted 3\n", "runner_host": "forge"});
            md.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            answer_of(&json!({"steps": [{"spec_slug": "execute", "status": "completed", "metadata": md}]}))
                .unwrap()
        };

        let shown = answered(json!({"effect": "acts: deleted 3, re-listed 7 left"}));
        let (line, ok) = verdict_line("abcd1234", &v, &shown);
        assert!(ok, "{line}");
        assert!(
            line.contains("exit 0")
                && line.contains("effect shown: acts: deleted 3, re-listed 7 left"),
            "{line}"
        );

        let unproven = answered(
            json!({"effect_unproven": "exit 0, and no line of the output matches the effect this verb declares (^acts: deleted)"}),
        );
        let (line, ok) = verdict_line("abcd1234", &v, &unproven);
        assert!(
            !ok,
            "an exit 0 whose effect was not shown is not a success: {line}"
        );
        assert!(
            line.contains("EFFECT NOT SHOWN") && line.contains("^acts: deleted"),
            "the verdict names what it looked for: {line}"
        );

        let unread = answered(json!({"effect_unread": "prints no re-list after it deletes"}));
        let (line, ok) = verdict_line("abcd1234", &v, &unread);
        assert!(
            ok,
            "a verb admitted without a read-back is not refused: {line}"
        );
        assert!(
            line.contains("EFFECT UNREAD") && line.contains("prints no re-list after it deletes"),
            "{line}"
        );

        // A read-only verb's answer carries none of the three, and reads
        // as it always did.
        let plain = answered(json!({}));
        let (line, ok) = verdict_line("abcd1234", &v, &plain);
        assert!(
            ok && !line.contains("EFFECT") && !line.contains("effect"),
            "{line}"
        );
    }

    /// AN ANSWER IS A COMPLETED STEP, NOT ITS KEYS (the adversarial
    /// review of car 24eb9471). The runner completes execute in two
    /// writes — its keys through the merge door, then the status alone
    /// (backlog 2aa2b19e) — so a status PUT the server refuses (the
    /// 3c3b202c unresolved-blockers class) or that never arrives leaves
    /// `disposition: answered, exit_code: 0` on an execute that is
    /// still open. `--wait` must not print "answered … exit 0" for a
    /// request nobody has answered.
    #[test]
    fn keys_merged_onto_an_open_execute_are_not_an_answer() {
        for status in ["ready", "active"] {
            let job = json!({"steps": [
                {"spec_slug": "execute", "status": status,
                 "metadata": {"disposition": "answered", "exit_code": "0",
                              "output": "ok\n", "runner_host": "forge"}},
                {"spec_slug": "refused", "status": "pending", "metadata": {"outcome_kind": "aborted"}}
            ]});
            assert!(
                answer_of(&job).is_none(),
                "an execute still {status} carrying merged keys is no answer"
            );
        }
    }

    /// A HASH HANDED TO `boss ops` IS REFUSED WITH ITS REASON AND THE
    /// FORM THAT FILES (backlog 2267b4f6). The rule moved to boss-jobs,
    /// where the website's door reads it too, and says what is wrong in
    /// no door's words; the command form is this door's, so it is held
    /// here, byte for byte the help main gave (Codex review of car
    /// 72e7a790).
    #[test]
    fn a_filer_supplied_plan_hash_is_refused_naming_who_supplies_it() {
        let v = shipped_allowlist().expect("the verb files parse");
        let hash = "0".repeat(64);
        let args = vec![
            "pvc-0a1b2c3d-0000-4000-8000-00000000abcd".to_string(),
            "pvc-0a1b2c3d-0000-4000-8000-00000000abcd-r-1234abcd".to_string(),
            hash.clone(),
        ];
        let e = validate(&v, "forge", "rebuild-a-failed-replica", &args)
            .unwrap_err()
            .to_string();
        assert_eq!(
            e,
            format!(
                "boss ops: REFUSED — `rebuild-a-failed-replica` takes 2 arg(s) (volume, \
                 replica), got 3: {args:?}. Its file declares one more, plan_sha256, and that \
                 one is never the filer's: the runner on forge renders the plan onto the \
                 packet's approve step and, after a passkey signs it, appends sha256 of the \
                 SIGNED plan itself, so a hash given here would be refused there (backlog \
                 fd7090cc). File it as: boss ops forge rebuild-a-failed-replica <volume> \
                 <replica>"
            )
        );
        // Every shipped approval verb names its own form.
        let table = v["verbs"].as_object().expect("a verbs table");
        for (name, spec) in table
            .iter()
            .filter(|(_, spec)| spec["requires_approval"] == serde_json::json!(true))
        {
            let host = spec["hosts"][0].as_str().expect("a host");
            let declared = spec["params"].as_array().expect("params").len();
            let mut args: Vec<String> = (1..declared).map(|i| format!("a{i}")).collect();
            args.push(hash.clone());
            let e = validate(&v, host, name, &args).unwrap_err().to_string();
            assert!(
                e.contains("a hash given here")
                    && e.contains(&format!("File it as: boss ops {host} {name}")),
                "{name}: {e}"
            );
        }
        // A plain arity error on a verb with no approval names no hash.
        let e = validate(&v, "forge", "uptime", &["x".into()])
            .unwrap_err()
            .to_string();
        assert!(
            e.starts_with("boss ops: REFUSED — `uptime` takes 0 arg(s)")
                && !e.contains("plan_sha256"),
            "{e}"
        );
    }
}
