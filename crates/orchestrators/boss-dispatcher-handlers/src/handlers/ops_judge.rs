//! `ops.judge` — an answered ops-request's verdict line files the next
//! verb, with args, so a follow-on that needs a word (`--for-real`)
//! rides a rule instead of a person.
//!
//! The gap this closes (backlog a1d3c762, measured in retro 27fad542):
//! the `publish-drift` verb's `--check` half rides a rule
//! (check-publish-drift-on-boss-gcp-converge-moved fires the moment
//! boss-gcp's checkout moves), but its `--for-real` half could not —
//! `jobs.job.closed` carries no metadata and no step output, and
//! `jobs.spawn` cannot pass an args list, so nothing could read the
//! answered `--check`'s verdict line and file the publish. 23 hand
//! publishes on 2026-09-18 13:4x were that gap. `maintenance.sweep.judge`
//! (970c0c94) already reads a verdict off a closed ops-request and
//! completes a step with it; this is the same shape, spawning instead of
//! completing, with every noun in the rule's args so the NEXT verb chain
//! is a rule file and not a handler.
//!
//! One half of that measurement has since been repaired: `jobs.spawn`
//! CAN pass an args list as of 4d53fae2 (2026-09-22), because the DSL
//! gained a list literal. What is still missing here is the reading —
//! `jobs.job.closed` carries no step output — so this handler's reason
//! to exist is the verdict line, not the args.
//!
//! ## The rule's args
//!
//! - `verb` — which answered ops-request this rule judges.
//! - `verdict_pattern` — a regex over the run step's recorded output,
//!   with NAMED groups; the LAST line it matches is the verdict. The arg
//!   passes through two string lexers (TOML, then boss-expr's), so write
//!   `[0-9]+` rather than `\d+`, which boss-expr's lexer reads as `d+`.
//! - `when` — a boss-expr predicate over the groups, each an int when it
//!   parses as one (`k = 0 AND n >= 1`). Measured: boss-expr evaluates
//!   its identifiers against any JSON object, so ONE definition of the
//!   predicate language serves rule `when`, step `ready_when`, and this.
//! - `then_verb`, `then_host`, `then_args` — the follow-on ops-request;
//!   `then_args` is whitespace-separated (a verb param admits none,
//!   `infra/ops/ops-runner.sh`), rendered as the JSON array the runner's
//!   argv contract reads off `metadata.args`. All three, or none: a rule
//!   with none is a WATCH (below).
//!
//! ## Watch mode (backlog 8d77d670)
//!
//! A rule that names no follow-on WATCHES a verb nobody files by hand —
//! `prune-registry-versions-daily`, a MUTATING delete a clock rule files
//! every day. Its request closes whether or not the verb did its job
//! (`answered` means the verb RAN, exit 2 and exit 1 included), and the
//! silence sweep reads only that a packet arrived, so a refused or
//! failed run was a closed packet nobody opened. A watch reads every
//! run and files one urgent alarm (`for_request`-keyed, as below) when
//! the run: FAILED (non-zero, not 75); was REFUSED by the ops runner (it
//! also fires on `outcome = "refused"`, which a chain ignores); exited 0
//! with no line matching `verdict_pattern`; or carries a verdict `when`
//! is false over. A run whose `when` holds is annotated `judged` and
//! nothing is filed. The alarm carries the verb, the trouble in words,
//! the line the run said, and the head of its output.
//!
//! ## What it does
//!
//! On `jobs.job.closed` for an `ops-request` that closed `answered`:
//!
//! 1. Read the closed request. It is this rule's only when its
//!    `metadata.verb` is `verb`; a request that IS the packet this rule
//!    would file (same verb, same args) is never judged by it, so a
//!    pattern that happened to match the follow-on's own answer could
//!    not chain forever.
//! 2. A request already carrying `metadata.judged` was judged by an
//!    earlier delivery (JetStream is at-least-once): nothing more.
//! 3. READ THE EXIT BEFORE THE OUTPUT. See below.
//! 4. Find the verdict — the last line of the `execute` step's `output`
//!    that matches `verdict_pattern`. None means the verb predates the
//!    verdict or was killed before its last line: warn, write nothing.
//! 5. Evaluate `when` over the groups. FALSE: write `judged` (the
//!    predicate, the groups, the line) onto the judged request and file
//!    nothing — a refusal is decided by a person, at the surface that
//!    shows it. TRUE: file the follow-on unless an open `then_verb`
//!    request already carries this request's id as `for_check` (or the
//!    same `for_converge`, when the judged request names one), then write
//!    `judged` naming what was filed. The spawned packet carries
//!    `for_check` = the judged request's id, so a reader follows the
//!    evidence from the publish to the check that earned it.
//!
//! ## The exit is read BEFORE the output (53f54b3f)
//!
//! This handler's whole job is to spend a MUTATING verb on the strength
//! of a read — `publish-drift --check` earns `publish-drift --for-real`,
//! which publishes workflow rows. Until this landed it judged the
//! output with no regard for whether the verb finished: every fixture
//! here hardcoded `exit_code: "0"`, so the failed case had never run.
//! A `--check` killed at its 1800s timeout, or one that died after
//! printing its table, leaves a PARTIAL output in the same merged field
//! — and a partial output that still carries a `refused 0` line reads
//! exactly like a clean one. Acting on a measurement that did not
//! complete is worse than failing to act, so a failed verb is not
//! judged at all: nothing is filed but the alarm, and the check is
//! annotated with the exit and the verb's last line so the reader who
//! opens it is not left to re-derive why the chain stopped.
//!
//! It ALERTS rather than only noting, because nothing else here is
//! holding the failure: the check closed, no step is open, no actor is
//! assigned, and the next converge files a fresh check that can fail
//! the same way forever — a check nobody reads is a check that is not
//! running (CLAUDE.md §Diagnosis). One alarm per failed request, keyed
//! `for_request`, the same dedup key `jobs.complete_linked_step` uses.
//! `maintenance.sweep.judge` answers the same question differently, and
//! says why in its own doc: its failure already has a reader.
//!
//! There is NO rule arg for this. The template it follows (f47861a5)
//! made its failure mode a free default with a named opt-out, because
//! there a rule could reasonably want the old note; here no rule could
//! reasonably want a `--for-real` chained off a measurement that did
//! not finish, so the behaviour is not a knob at all — a rule cannot
//! forget to ask for it, and no future rule file can reintroduce the
//! defect (CLAUDE.md §9a).

use super::common::{api_client, get_json, open_jobs_of_kind, row_or_refuse, write_json};
use super::jobs_complete_linked_step::{FOR_REQUEST, VerbFailure, step_by_slug, verb_failure};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::{self, Value};
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use boss_jobs::channels::InputChannel;
use serde_json::json;
use std::sync::Arc;

/// The step the ops-runner completes with the verb's output.
pub(crate) const REPORT_STEP: &str = "execute";
/// The key this handler writes onto a judged request, and reads first
/// on a redelivery.
pub(crate) const JUDGED: &str = "judged";
/// The link a filed follow-on carries back to the request it judged.
pub(crate) const FOR_CHECK: &str = "for_check";

/// The ops-request a chain files when its `when` holds.
pub(crate) struct FollowOn {
    pub verb: String,
    pub host: String,
    pub args: Vec<String>,
}

/// One rule's declaration, parsed off its args. A bad regex or a bad
/// predicate is rule authoring, identical on every redelivery, so both
/// are `Permanent`. `then` is `None` for a WATCH (see the module doc).
pub(crate) struct Judgement {
    pub verb: String,
    pub pattern: regex::Regex,
    pub when_src: String,
    pub when: expr::Expr,
    pub then: Option<FollowOn>,
}

/// The three follow-on args: all present (a chain) or all absent (a
/// watch). Half a follow-on is rule authoring, so `Permanent`.
const THEN_ARGS: [&str; 3] = ["then_verb", "then_host", "then_args"];

impl Judgement {
    pub(crate) fn from_args(args: &[(String, Value)]) -> Result<Self, HandlerError> {
        let given: Vec<&str> = THEN_ARGS
            .into_iter()
            .filter(|k| boss_dispatcher::rules::handler::arg(args, k).is_some())
            .collect();
        let then = match given.len() {
            0 => None,
            3 => Some(FollowOn {
                verb: arg_string(args, "then_verb")?.to_string(),
                host: arg_string(args, "then_host")?.to_string(),
                args: arg_string(args, "then_args")?
                    .split_whitespace()
                    .map(str::to_string)
                    .collect(),
            }),
            _ => {
                let missing: Vec<&str> = THEN_ARGS
                    .into_iter()
                    .filter(|k| !given.contains(k))
                    .collect();
                return Err(HandlerError::Permanent(format!(
                    "a follow-on names all of {THEN_ARGS:?} or none (a watch); this rule gives \
                     {given:?} and lacks {missing:?}"
                )));
            }
        };
        let verb = arg_string(args, "verb")?.to_string();
        let pattern_src = arg_string(args, "verdict_pattern")?;
        let pattern = regex::Regex::new(pattern_src).map_err(|e| {
            HandlerError::Permanent(format!(
                "verdict_pattern {pattern_src:?} is not a regex: {e}"
            ))
        })?;
        let when_src = arg_string(args, "when")?.to_string();
        let when = expr::parse(&when_src).map_err(|e| {
            HandlerError::Permanent(format!("when {when_src:?} does not parse: {e}"))
        })?;
        Ok(Self {
            verb,
            pattern,
            when_src,
            when,
            then,
        })
    }
}

/// PURE: the verdict a recorded output carries — the LAST line the
/// pattern matches, as (the line, its named groups as JSON). A group
/// that parses as an integer is one, so `k = 0` compares numbers; any
/// other group is a string. `None` when no line matches.
pub(crate) fn verdict_groups(
    pattern: &regex::Regex,
    output: &str,
) -> Option<(String, serde_json::Value)> {
    let line = output
        .lines()
        .map(str::trim)
        .rfind(|l| pattern.is_match(l))?;
    let caps = pattern.captures(line)?;
    let groups: serde_json::Map<String, serde_json::Value> = pattern
        .capture_names()
        .flatten()
        .filter_map(|name| {
            let m = caps.name(name)?;
            let v = match m.as_str().parse::<i64>() {
                Ok(i) => json!(i),
                Err(_) => json!(m.as_str()),
            };
            Some((name.to_string(), v))
        })
        .collect();
    Some((line.to_string(), serde_json::Value::Object(groups)))
}

/// PURE: does the rule's `when` hold over these groups? A predicate that
/// evaluates to something other than a bool (a bare identifier, a
/// missing group compared as absent is fine — that is false) is rule
/// authoring, so `Permanent`.
pub(crate) fn when_holds(
    when: &expr::Expr,
    when_src: &str,
    groups: &serde_json::Value,
) -> Result<bool, HandlerError> {
    let ctx = expr::Context {
        payload: groups,
        helpers: &expr::NoHelpers,
    };
    match expr::eval(when, &ctx) {
        Ok(v) => v.as_bool().ok_or_else(|| {
            HandlerError::Permanent(format!(
                "when {when_src:?} evaluated to {} over {groups}, not a bool",
                v.kind()
            ))
        }),
        Err(e) => Err(HandlerError::Permanent(format!(
            "when {when_src:?} failed over {groups}: {e}"
        ))),
    }
}

/// PURE: the follow-on ops-request, in the shape the ops-runner reads
/// (`metadata.host`, `metadata.verb`, `metadata.args` as a JSON array of
/// strings) plus the links a reader follows back.
pub(crate) fn follow_on_body(
    j: &Judgement,
    then: &FollowOn,
    judged_id: &str,
    judged: &serde_json::Value,
    verdict: &str,
    ctx: &InvocationContext,
) -> serde_json::Value {
    let short = &judged_id[..judged_id.len().min(8)];
    let args = then.args.join(" ");
    let mut metadata = json!({
        "host": then.host,
        "verb": then.verb,
        "args": then.args,
        FOR_CHECK: judged_id,
        "judged_verdict": verdict,
        "spawned_by_rule": ctx.rule_name,
        "triggered_by_event_id": ctx.triggering_event_id,
        "triggered_by_topic": ctx.triggering_topic,
    });
    if let (Some(fc), Some(m)) = (meta_str(judged, "for_converge"), metadata.as_object_mut()) {
        m.insert("for_converge".into(), json!(fc));
    }
    json!({
        "kind": "ops-request",
        "title": format!(
            "{} {args} on {} — filed on {}'s answer (ops-request {short})",
            then.verb, then.host, j.verb
        ),
        "subject": {"subject_kind": "custom", "id": then.host},
        "owner_id": format!("rule:{}", ctx.rule_name),
        "priority": "standard",
        "status": "open",
        "tags": ["dispatcher-spawned"],
        "metadata": metadata,
    })
}

/// PURE: the urgent packet a failed measurement becomes (53f54b3f) —
/// the verb that failed, its exit, its last line verbatim, and the
/// follow-on that was NOT filed because of it. `for_request` is the
/// dedup key, the same one `jobs.complete_linked_step` writes, so one
/// failed request grows one alarm however many times it is delivered.
/// Subject: the judged request's own, so "what went wrong on this host"
/// answers from Subject history.
pub(crate) fn chain_refused_alert_body(
    j: &Judgement,
    follow_on: &FollowOn,
    judged_id: &str,
    judged: &serde_json::Value,
    failure: &VerbFailure,
    owner: &str,
    ctx: &InvocationContext,
) -> serde_json::Value {
    let short = judged_id.get(..8).unwrap_or(judged_id);
    let then = format!("{} {}", follow_on.verb, follow_on.args.join(" "));
    let host = meta_str(judged, "host").unwrap_or(&follow_on.host);
    let subject = judged
        .get("subject")
        .filter(|s| {
            s.get("id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| !id.is_empty())
        })
        .cloned()
        .unwrap_or_else(|| json!({ "subject_kind": "custom", "id": host }));
    json!({
        "kind": "backlog-item",
        "title": format!(
            "{} FAILED (exit {}) on ops-request {short}: {} was not filed",
            j.verb, failure.exit, then.trim()
        ),
        "subject": subject,
        // The platform owner as the registry answers it, or nobody for
        // the jobs API to resolve from the kind's owner_role (3c23662d).
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": {
            "input_channel": super::common::lane_label(InputChannel::PipelineFailure),
            "area": "platform",
            FOR_REQUEST: judged_id,
            "verb": j.verb,
            "exit": failure.exit,
            "failed": failure.line,
            "reporter": ctx.rule_name,
            "triggered_by_event_id": ctx.triggering_event_id,
            "detail": format!(
                "Filed by {} (backlog 53f54b3f): ops-request {short} ran `{}` on {host} and the \
                 verb FAILED (exit {}); the request closed `answered`, because the verb ran. This \
                 rule spends `{}` on the strength of that read, so it was NOT filed — a \
                 measurement that did not complete cannot earn a mutating follow-on, and a \
                 truncated output can still carry a line that reads clean. Nothing was judged and \
                 nothing was published; the chain stays stopped until someone re-runs the \
                 measurement. The verb's last line: {}",
                ctx.rule_name, j.verb, failure.exit, then.trim(), failure.line
            ),
        },
    })
}

/// Why a WATCHED run is an alarm (backlog 8d77d670). Each is a run that
/// did not do its job, in the one place anything reads it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Trouble {
    /// The verb RAN and exited non-zero (not 75, the estate's not-yet).
    Failed(VerbFailure),
    /// The ops runner refused to run it; the runner's reason, verbatim.
    RunnerRefused(String),
    /// Exit 0, and no line of the output matches `verdict_pattern`.
    NoVerdict,
    /// The verdict was found, and `when` is false over it.
    VerdictFalse {
        verdict: String,
        groups: serde_json::Value,
    },
}

impl Trouble {
    fn exit(&self) -> Option<&str> {
        match self {
            Self::Failed(f) => Some(&f.exit),
            _ => None,
        }
    }

    /// The line an alarm quotes as what the run said.
    fn line(&self) -> String {
        match self {
            Self::Failed(f) => f.line.clone(),
            Self::RunnerRefused(reason) => reason.clone(),
            Self::NoVerdict => String::new(),
            Self::VerdictFalse { verdict, .. } => verdict.clone(),
        }
    }

    fn headline(&self, verb: &str, when_src: &str) -> String {
        match self {
            Self::Failed(f) => format!("{verb} FAILED (exit {})", f.exit),
            Self::RunnerRefused(_) => format!("{verb} was REFUSED by the ops runner"),
            Self::NoVerdict => format!("{verb} answered with no verdict"),
            Self::VerdictFalse { .. } => format!("{verb}'s verdict fails `{when_src}`"),
        }
    }

    fn why(&self, when_src: &str, pattern: &str) -> String {
        match self {
            Self::Failed(f) => format!("the verb FAILED (exit {}); its line: {}", f.exit, f.line),
            Self::RunnerRefused(reason) => {
                format!("the ops runner REFUSED to run it, so nothing ran: {reason}")
            }
            Self::NoVerdict => format!(
                "it exited 0 with no verdict — no line of its output matches {pattern:?} — and \
                 no evidence is not a pass"
            ),
            Self::VerdictFalse { verdict, groups } => {
                format!("its verdict fails `{when_src}` over {groups}: {verdict}")
            }
        }
    }
}

/// The runner's recorded output on the `execute` step, or "".
fn execute_output(job: &serde_json::Value) -> &str {
    step_by_slug(job, REPORT_STEP)
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("output"))
        .and_then(|o| o.as_str())
        .unwrap_or("")
}

/// How much of a watched run's output an alarm carries: its HEAD, where
/// a verb that puts its verdict first (prune-registry-versions: the
/// record, then the summary, then the per-version list) says what it
/// did. The whole output stays on the request's execute step.
const OUTPUT_HEAD_LINES: usize = 40;
const OUTPUT_HEAD_CHARS: usize = 4000;

fn output_head(output: &str) -> String {
    let head = output
        .lines()
        .take(OUTPUT_HEAD_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    head.chars().take(OUTPUT_HEAD_CHARS).collect()
}

/// PURE: the urgent packet a troubled WATCHED run becomes — the verb,
/// the trouble in words, the line the run said, and the head of its
/// output. `for_request` is the dedup key, as for a chain's alarm.
pub(crate) fn watch_alert_body(
    j: &Judgement,
    judged_id: &str,
    judged: &serde_json::Value,
    trouble: &Trouble,
    owner: &str,
    ctx: &InvocationContext,
) -> serde_json::Value {
    let short = judged_id.get(..8).unwrap_or(judged_id);
    let host = meta_str(judged, "host").unwrap_or("?");
    let subject = judged
        .get("subject")
        .filter(|s| {
            s.get("id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| !id.is_empty())
        })
        .cloned()
        .unwrap_or_else(|| json!({ "subject_kind": "custom", "id": host }));
    let mut metadata = json!({
        "area": "platform",
        FOR_REQUEST: judged_id,
        "verb": j.verb,
        "failed": trouble.line(),
        "output_head": output_head(execute_output(judged)),
        "reporter": ctx.rule_name,
        "triggered_by_event_id": ctx.triggering_event_id,
        "detail": format!(
            "Filed by {} (backlog 8d77d670): ops-request {short} ran `{}` on {host}, a verb a rule \
             files with nobody reading the result, and {}. This watch is the one reader of every \
             run: the request closes whether or not the verb did its job, so without this alarm \
             a run that did nothing looks exactly like one that did. The full output is on the \
             request's execute step; its head is in output_head.",
            ctx.rule_name,
            j.verb,
            trouble.why(&j.when_src, j.pattern.as_str())
        ),
    });
    if let (Some(exit), Some(m)) = (trouble.exit(), metadata.as_object_mut()) {
        m.insert("exit".into(), json!(exit));
    }
    json!({
        "kind": "backlog-item",
        "title": format!(
            "{} on ops-request {short}",
            trouble.headline(&j.verb, &j.when_src)
        ),
        "subject": subject,
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": super::common::with_lane(metadata, InputChannel::PipelineFailure),
    })
}

/// PURE: is `open` the follow-on this judgement already filed for
/// `judged` — same verb, linked by `for_check` or by a shared
/// `for_converge`?
pub(crate) fn already_filed(
    then: &FollowOn,
    judged_id: &str,
    judged: &serde_json::Value,
    open: &serde_json::Value,
) -> bool {
    if meta_str(open, "verb") != Some(&then.verb) {
        return false;
    }
    if meta_str(open, FOR_CHECK) == Some(judged_id) {
        return true;
    }
    match meta_str(judged, "for_converge") {
        Some(fc) => meta_str(open, "for_converge") == Some(fc),
        None => false,
    }
}

fn meta_str<'a>(job: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    job.get("metadata")?
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn meta_args(job: &serde_json::Value) -> Vec<String> {
    job.get("metadata")
        .and_then(|m| m.get("args"))
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

pub struct OpsJudge {
    client: boss_core::machine_token::Client,
    jobs_base: String,
    /// Who a failed-verb alarm is addressed to — the platform owner as
    /// the people registry answers it, like every other filer here.
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl OpsJudge {
    pub fn new(
        jobs_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            owner,
        })
    }

    /// Tests point the client at a local stand-in for jobs-api.
    pub fn with_client(
        client: boss_core::machine_token::Client,
        jobs_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
            owner,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    async fn job(&self, id: &str, rule: &str) -> Result<serde_json::Value, HandlerError> {
        let job = get_json(
            &self.client,
            &format!("{}/api/jobs/{id}", self.base()),
            rule,
        )
        .await?;
        row_or_refuse(job, &format!("GET /api/jobs/{id}")).map_err(HandlerError::Downstream)
    }

    /// POST the follow-on and read the id the jobs API minted for it —
    /// the thing the judged request's `judged` note names.
    async fn file(&self, body: &serde_json::Value, rule: &str) -> Result<String, HandlerError> {
        super::common::post_json_minted_id(
            &self.client,
            &format!("{}/api/jobs", self.base()),
            body,
            rule,
        )
        .await
    }

    /// One alarm per troubled request, keyed `for_request` (the key
    /// `jobs.complete_linked_step` writes too): an open one is reused, so
    /// a redelivery after the alarm landed but before the note did files
    /// no twin. Returns the alarm's id.
    async fn find_or_file_alarm(
        &self,
        judged_id: &str,
        rule: &str,
        body: impl FnOnce(&str) -> serde_json::Value,
    ) -> Result<String, HandlerError> {
        let open = open_jobs_of_kind(&self.client, self.base(), "backlog-item", rule).await?;
        match open
            .iter()
            .find(|job| meta_str(job, FOR_REQUEST) == Some(judged_id))
            .and_then(|job| job.get("id").and_then(|v| v.as_str()))
        {
            Some(existing) => Ok(existing.to_string()),
            None => {
                let owner = super::common::owner_for_filing(self.owner.as_ref(), rule).await;
                self.file(&body(&owner), rule).await
            }
        }
    }

    /// THE FAILED MEASUREMENT (53f54b3f). The verb this rule judges RAN
    /// and FAILED, so its output is a partial reading and the mutating
    /// follow-on is not filed. The alarm goes FIRST and the note names
    /// it: a redelivery after the alarm landed but before the note did
    /// finds it open by `for_request` and reuses it, and one after both
    /// landed stops at `judged`. Nothing about the judged request is
    /// completed or routed — the chain stays stopped, which is the
    /// point.
    async fn refuse_to_chain(
        &self,
        j: &Judgement,
        then: &FollowOn,
        judged_id: &str,
        judged: &serde_json::Value,
        failure: &VerbFailure,
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let alert_id = self
            .find_or_file_alarm(judged_id, rule, |owner| {
                chain_refused_alert_body(j, then, judged_id, judged, failure, owner, ctx)
            })
            .await?;
        self.annotate(
            judged_id,
            json!({
                JUDGED: format!(
                    "{rule}: nothing filed — {} FAILED (exit {}), so its reading is partial and \
                     `{} {}` cannot ride it; alarm {alert_id}. The verb's last line: {}",
                    j.verb,
                    failure.exit,
                    then.verb,
                    then.args.join(" "),
                    failure.line
                ),
                "failed": failure.line,
                "failed_exit": failure.exit,
                "alert": alert_id,
            }),
            rule,
        )
        .await?;
        tracing::warn!(
            rule = %rule,
            judged = %judged_id,
            alert = %alert_id,
            "{} FAILED (exit {}) — {} {} NOT filed; {}",
            j.verb,
            failure.exit,
            then.verb,
            then.args.join(" "),
            failure.line
        );
        Ok(())
    }

    /// A WATCH's alarm (backlog 8d77d670): the watched run did not do
    /// its job, and nothing else reads it. Same ordering as
    /// `refuse_to_chain`: the alarm first, then the note naming it.
    async fn raise(
        &self,
        j: &Judgement,
        judged_id: &str,
        judged: &serde_json::Value,
        trouble: &Trouble,
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let alert_id = self
            .find_or_file_alarm(judged_id, rule, |owner| {
                watch_alert_body(j, judged_id, judged, trouble, owner, ctx)
            })
            .await?;
        let mut note = json!({
            JUDGED: format!(
                "{rule}: ALARM {alert_id} — {}",
                trouble.why(&j.when_src, j.pattern.as_str())
            ),
            "failed": trouble.line(),
            "alert": alert_id,
        });
        if let (Some(exit), Some(m)) = (trouble.exit(), note.as_object_mut()) {
            m.insert("failed_exit".into(), json!(exit));
        }
        self.annotate(judged_id, note, rule).await?;
        tracing::warn!(
            rule = %rule,
            judged = %judged_id,
            alert = %alert_id,
            "watched {}: {}",
            j.verb,
            trouble.why(&j.when_src, j.pattern.as_str())
        );
        Ok(())
    }

    async fn annotate(
        &self,
        judged_id: &str,
        note: serde_json::Value,
        rule: &str,
    ) -> Result<(), HandlerError> {
        write_json(
            &self.client,
            reqwest::Method::PATCH,
            &format!("{}/api/jobs/{judged_id}/metadata", self.base()),
            &note,
            rule,
        )
        .await
    }
}

#[async_trait]
impl Handler for OpsJudge {
    fn name(&self) -> &'static str {
        "ops.judge"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let j = Judgement::from_args(args)?;
        let rule = ctx.rule_name.as_str();

        // The close marker names the packet. A malformed marker is not
        // something a redelivery can fix, so it is a no-op, not an error.
        let Some(judged_id) = ctx.event_payload.get("id").and_then(|v| v.as_str()) else {
            return Ok(());
        };

        // 1. The judged request — this rule's only if it answered the
        //    verb this rule reads, and is not the packet this rule files.
        let judged = self.job(judged_id, rule).await?;
        if meta_str(&judged, "verb") != Some(&j.verb) {
            return Ok(());
        }
        if let Some(then) = &j.then
            && j.verb == then.verb
            && meta_args(&judged) == then.args
        {
            tracing::debug!(rule = %rule, judged = %judged_id, "answered {} is the follow-on this rule files, not the request it judges", j.verb);
            return Ok(());
        }

        // 2. Judged by an earlier delivery: the record is true.
        if meta_str(&judged, JUDGED).is_some() {
            return Ok(());
        }

        // 2b. A RUNNER refusal ran nothing and closed the request
        //     `refused`. A chain has nothing to read in it; a watch's
        //     daily bound did not hold, so it is an alarm.
        let refused = meta_str(&judged, "outcome") == Some("refused")
            || ctx.event_payload.get("outcome").and_then(|v| v.as_str()) == Some("refused");
        if refused {
            if j.then.is_some() {
                return Ok(());
            }
            let reason = step_by_slug(&judged, REPORT_STEP)
                .and_then(|s| s.get("metadata"))
                .and_then(|m| m.get("reason"))
                .and_then(|r| r.as_str())
                .unwrap_or("the ops runner refused it and recorded no reason")
                .to_string();
            return self
                .raise(&j, judged_id, &judged, &Trouble::RunnerRefused(reason), ctx)
                .await;
        }

        // 3. THE EXIT, BEFORE THE OUTPUT (53f54b3f). A verb that RAN
        //    and FAILED left a partial measurement, and this rule
        //    spends a MUTATING follow-on on the strength of it. A
        //    `--check` killed at its timeout with its table already
        //    merged into the same field still carries a `refused 0`
        //    line, so `when` would hold and the publish would ride a
        //    reading that never finished. Nothing is judged; the alarm
        //    is filed and the refusal written onto the check. A watch
        //    alarms on the same fact, in its own words.
        if let Some(failure) = verb_failure(&judged) {
            return match &j.then {
                Some(then) => {
                    self.refuse_to_chain(&j, then, judged_id, &judged, &failure, ctx)
                        .await
                }
                None => {
                    self.raise(&j, judged_id, &judged, &Trouble::Failed(failure), ctx)
                        .await
                }
            };
        }

        // 4. The verdict, off the runner's recorded output.
        let output = execute_output(&judged);
        let Some((verdict, groups)) = verdict_groups(&j.pattern, output) else {
            if j.then.is_none() {
                // No evidence is not a pass: a watched run that answered
                // without its verdict is a run nobody can vouch for.
                return self
                    .raise(&j, judged_id, &judged, &Trouble::NoVerdict, ctx)
                    .await;
            }
            tracing::warn!(
                rule = %rule,
                judged = %judged_id,
                "{} answered without a line matching {:?} — the verb predates the verdict, or was cut short; nothing judged, nothing filed",
                j.verb, j.pattern.as_str()
            );
            return Ok(());
        };

        // 5. The decision.
        let holds = when_holds(&j.when, &j.when_src, &groups)?;
        let Some(then) = &j.then else {
            if !holds {
                let trouble = Trouble::VerdictFalse { verdict, groups };
                return self.raise(&j, judged_id, &judged, &trouble, ctx).await;
            }
            self.annotate(
                judged_id,
                json!({
                    JUDGED: format!(
                        "{rule}: watched — {} holds over {groups}; nothing to file; verdict: {verdict}",
                        j.when_src
                    ),
                    "judged_verdict": verdict,
                }),
                rule,
            )
            .await?;
            tracing::info!(rule = %rule, judged = %judged_id, "{verdict} — {} holds; watched, nothing to file", j.when_src);
            return Ok(());
        };
        if !holds {
            self.annotate(
                judged_id,
                json!({
                    JUDGED: format!(
                        "{rule}: nothing filed — {} is false over {groups}; verdict: {verdict}",
                        j.when_src
                    ),
                    "judged_verdict": verdict,
                }),
                rule,
            )
            .await?;
            tracing::info!(rule = %rule, judged = %judged_id, "{verdict} — {} is false over {groups}; nothing filed", j.when_src);
            return Ok(());
        }

        let open = open_jobs_of_kind(&self.client, self.base(), "ops-request", rule).await?;
        let filed_id = match open
            .iter()
            .find(|o| already_filed(then, judged_id, &judged, o))
            .and_then(|o| o.get("id").and_then(|v| v.as_str()))
        {
            // Filed by an earlier delivery whose note never landed, or
            // by the same converge's other check: the record still owes
            // the judged request its note.
            Some(existing) => existing.to_string(),
            None => {
                let body = follow_on_body(&j, then, judged_id, &judged, &verdict, ctx);
                self.file(&body, rule).await?
            }
        };
        self.annotate(
            judged_id,
            json!({
                JUDGED: format!(
                    "{rule}: filed {} {} on {} as ops-request {filed_id} — {} is true over {groups}; verdict: {verdict}",
                    then.verb,
                    then.args.join(" "),
                    then.host,
                    j.when_src
                ),
                "judged_verdict": verdict,
                "filed": filed_id,
            }),
            rule,
        )
        .await?;
        tracing::info!(rule = %rule, judged = %judged_id, filed = %filed_id, "{verdict} — filed {} {} on {}", then.verb, then.args.join(" "), then.host);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::Path, extract::Query, routing::get};
    use std::collections::HashMap;
    use std::sync::Mutex;

    const CHECK: &str = "11111111-1111-1111-1111-111111111111";
    const CONVERGE: &str = "33333333-3333-3333-3333-333333333333";
    const RULE: &str = "publish-drift-for-real-on-clean-check";
    /// The rule's pattern, as the rule file spells it (`[0-9]+`, not
    /// `\d+` — see the module doc).
    const PATTERN: &str = "publish-drift: would publish (?P<n>[0-9]+), skipped (?P<m>[0-9]+) equal, refused (?P<k>[0-9]+)";
    const WHEN: &str = "k = 0 AND n >= 1";

    fn ctx() -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: RULE.into(),
            triggering_event_id: "evt-close-1".into(),
            triggering_topic: "jobs.job.closed".into(),
            // The close marker in the shape all three emit sites produce:
            // every key present, and NO step metadata.
            event_payload: json!({
                "id": CHECK,
                "closed_on": "2026-09-18",
                "kind": "ops-request",
                "outcome": "answered",
                "title": "publish-drift --check on boss-gcp — its checkout moved",
                "subject_id": "boss-gcp",
                "parent_step_id": null,
            }),
        }
    }

    fn args() -> Vec<(String, Value)> {
        [
            ("verb", "publish-drift"),
            ("verdict_pattern", PATTERN),
            ("when", WHEN),
            ("then_verb", "publish-drift"),
            ("then_host", "boss-gcp"),
            ("then_args", "--for-real"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.into())))
        .collect()
    }

    /// The answered request, as the ops-runner completes it — the verb
    /// having exited 0.
    fn request(id: &str, verb: &str, req_args: &[&str], output: &str) -> serde_json::Value {
        request_exit(id, verb, req_args, output, "0")
    }

    /// The same, with the exit the runner recorded on the execute step
    /// (53f54b3f): every fixture here hardcoded `"0"`, which is why no
    /// test had ever driven this handler with a verb that failed.
    fn request_exit(
        id: &str,
        verb: &str,
        req_args: &[&str],
        output: &str,
        exit: &str,
    ) -> serde_json::Value {
        json!({
            "id": id,
            "kind": "ops-request",
            "status": "closed",
            // The exit is recorded ONCE, on the execute step below
            // (50fede8b collapsed the request-level `metadata.exit`
            // the runner used to write beside it); this handler reads
            // it there — see the module doc.
            "metadata": { "host": "boss-gcp", "verb": verb, "args": req_args, "for_converge": CONVERGE },
            "steps": [
                { "id": "r-filed", "spec_slug": "filed", "status": "completed", "metadata": {} },
                { "id": "r-execute", "spec_slug": "execute", "status": "completed",
                  "completed_at": "2026-09-18T15:02:00Z",
                  "metadata": { "disposition": "answered", "exit_code": exit, "runner_host": "boss-gcp",
                                "output": output, "authority_role": "platform-admin" } },
                { "id": "r-answered", "spec_slug": "answered", "status": "completed", "metadata": {} },
            ],
        })
    }

    /// Every write the handler made: (method, path, body), in order.
    type Writes = Arc<Mutex<Vec<(String, String, serde_json::Value)>>>;

    /// Stand-in for jobs-api: `GET /api/jobs/{id}` serves one, `GET
    /// /api/jobs?kind=&status=open` lists the open ones, `POST /api/jobs`
    /// mints an id and keeps the row open, `PATCH /api/jobs/{id}/metadata`
    /// merges — so a second delivery reads what the first wrote.
    async fn mock_jobs(jobs: Vec<serde_json::Value>) -> (String, Writes) {
        let writes: Writes = Arc::new(Mutex::new(Vec::new()));
        let by_id: Arc<Mutex<HashMap<String, serde_json::Value>>> = Arc::new(Mutex::new(
            jobs.into_iter()
                .map(|j| (j["id"].as_str().unwrap_or_default().to_string(), j))
                .collect(),
        ));
        let (g, l, pj, pm) = (by_id.clone(), by_id.clone(), by_id.clone(), by_id.clone());
        let (wj, wm) = (writes.clone(), writes.clone());
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = g.clone();
                    async move {
                        by_id
                            .lock()
                            .unwrap()
                            .get(&id)
                            .cloned()
                            .map(Json)
                            .ok_or(axum::http::StatusCode::NOT_FOUND)
                    }
                }),
            )
            .route(
                "/api/jobs",
                get(move |Query(q): Query<HashMap<String, String>>| {
                    let by_id = l.clone();
                    async move {
                        let rows: Vec<serde_json::Value> = by_id
                            .lock()
                            .unwrap()
                            .values()
                            .filter(|j| {
                                q.get("kind").is_none_or(|k| j["kind"] == json!(k))
                                    && q.get("status").is_none_or(|s| j["status"] == json!(s))
                            })
                            .cloned()
                            .collect();
                        Json(json!({ "data": rows, "total": rows.len() }))
                    }
                })
                .post(move |Json(body): Json<serde_json::Value>| {
                    let (w, by_id) = (wj.clone(), pj.clone());
                    async move {
                        w.lock()
                            .unwrap()
                            .push(("POST".into(), "/api/jobs".into(), body.clone()));
                        let mut map = by_id.lock().unwrap();
                        let id = format!("f0000000-0000-0000-0000-{:012}", map.len() + 1);
                        let mut row = body;
                        row["id"] = json!(id);
                        map.insert(id.clone(), row);
                        (axum::http::StatusCode::CREATED, Json(json!({ "id": id })))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(
                    move |Path(id): Path<String>, Json(body): Json<serde_json::Value>| {
                        let (w, by_id) = (wm.clone(), pm.clone());
                        async move {
                            w.lock().unwrap().push((
                                "PATCH".into(),
                                format!("/api/jobs/{id}/metadata"),
                                body.clone(),
                            ));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id)
                                && let (Some(m), Some(b)) =
                                    (job["metadata"].as_object_mut(), body.as_object())
                            {
                                for (k, v) in b {
                                    m.insert(k.clone(), v.clone());
                                }
                            }
                            axum::http::StatusCode::NO_CONTENT
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), writes)
    }

    /// The verb's answer, as publish-drift.sh prints it (the parked car
    /// f1b2822a): a table, then ONE verdict line with a trailing
    /// parenthetical the pattern does not need to consume.
    const CLEAN_OUTPUT: &str = "publish-drift: checkout cb053ed6 on boss-gcp\n\nkind\tfrom\tto\tresult\nmaintenance-sweep\tv3\ttree\twould publish\n\npublish-drift: would publish 3, skipped 20 equal, refused 0 (checkout cb053ed6, 23 kind(s), packet 11111111)\n";
    const REFUSED_OUTPUT: &str = "kind\tfrom\tto\tresult\nbacklog-item\tv9\t-\tREFUSED: live v9 carries what the tree never said\n\npublish-drift: would publish 3, skipped 19 equal, refused 1 (checkout cb053ed6, 23 kind(s), packet 11111111)\n";
    const NOTHING_AHEAD_OUTPUT: &str = "publish-drift: would publish 0, skipped 23 equal, refused 0 (checkout cb053ed6, 23 kind(s), packet 11111111)\n";
    const FOR_REAL_OUTPUT: &str = "publish-drift: published 3, skipped 20 equal, refused 0 (checkout cb053ed6, 23 kind(s), packet f0000000)\n";

    /// The handler under test, with a fixed platform owner — the id
    /// an alarm it files is addressed to.
    fn judge(base: String) -> Arc<OpsJudge> {
        OpsJudge::with_client(
            crate::handlers::common::api_client(),
            base,
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    fn posts(w: &[(String, String, serde_json::Value)]) -> Vec<serde_json::Value> {
        w.iter()
            .filter(|(m, _, _)| m == "POST")
            .map(|(_, _, b)| b.clone())
            .collect()
    }

    /// A clean check files the follow-on with its args as the runner's
    /// JSON array, linked back by `for_check`, and notes on the check
    /// what was filed.
    #[tokio::test]
    async fn a_clean_check_files_the_follow_on_with_args_and_for_check() {
        let (base, writes) =
            mock_jobs(vec![request(CHECK, "publish-drift", &[], CLEAN_OUTPUT)]).await;
        let h = judge(base);
        h.invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 2, "one spawn + one note, nothing else: {w:?}");
        let filed = &posts(&w)[0];
        assert_eq!(filed["kind"], "ops-request");
        assert_eq!(
            filed["subject"],
            json!({"subject_kind": "custom", "id": "boss-gcp"})
        );
        let m = &filed["metadata"];
        assert_eq!(m["host"], "boss-gcp");
        assert_eq!(m["verb"], "publish-drift");
        assert_eq!(
            m["args"],
            json!(["--for-real"]),
            "the runner's argv contract: a JSON array of strings"
        );
        assert_eq!(m[FOR_CHECK], CHECK, "the evidence link back to the check");
        assert_eq!(
            m["for_converge"], CONVERGE,
            "the check's own link rides through"
        );
        assert_eq!(m["spawned_by_rule"], RULE);
        assert!(
            m["judged_verdict"]
                .as_str()
                .is_some_and(|v| v
                    .starts_with("publish-drift: would publish 3, skipped 20 equal, refused 0")),
            "the verdict line, copied: {m}"
        );
        assert_eq!(filed["owner_id"], format!("rule:{RULE}"));
        assert_eq!(
            (w[1].0.as_str(), w[1].1.as_str()),
            ("PATCH", &*format!("/api/jobs/{CHECK}/metadata"))
        );
        let note = w[1].2[JUDGED].as_str().unwrap_or("");
        assert!(
            note.contains("filed publish-drift --for-real on boss-gcp as ops-request f0000000-0000-0000-0000-000000000002"),
            "the note names what was filed: {note}"
        );
        assert_eq!(w[1].2["filed"], "f0000000-0000-0000-0000-000000000002");
    }

    /// A refusal files nothing: the check is annotated with the
    /// predicate that was false and the line it was false over, and the
    /// decision stays a person's at the Drift tab.
    #[tokio::test]
    async fn a_refused_check_files_nothing_and_annotates_the_check() {
        let (base, writes) =
            mock_jobs(vec![request(CHECK, "publish-drift", &[], REFUSED_OUTPUT)]).await;
        let h = judge(base);
        h.invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 1, "one note, no spawn: {w:?}");
        assert_eq!(
            (w[0].0.as_str(), w[0].1.as_str()),
            ("PATCH", &*format!("/api/jobs/{CHECK}/metadata"))
        );
        let note = w[0].2[JUDGED].as_str().unwrap_or("");
        assert!(note.contains("nothing filed"), "{note}");
        assert!(note.contains(WHEN), "names the predicate: {note}");
        assert!(
            note.contains("\"k\":1"),
            "and the groups it was false over: {note}"
        );
        assert!(note.contains("refused 1"), "and the line: {note}");
        assert!(w[0].2.get("filed").is_none());
    }

    /// Nothing ahead is not a refusal, but `n >= 1` is false: nothing to
    /// publish, nothing filed, and the check says so.
    #[tokio::test]
    async fn a_check_with_nothing_ahead_files_nothing() {
        let (base, writes) = mock_jobs(vec![request(
            CHECK,
            "publish-drift",
            &[],
            NOTHING_AHEAD_OUTPUT,
        )])
        .await;
        let h = judge(base);
        h.invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(w[0].0, "PATCH");
        assert!(posts(&w).is_empty());
    }

    /// The verb predates the verdict (or was killed before its last
    /// line): nothing is written and nothing is guessed.
    #[tokio::test]
    async fn an_answer_without_a_matching_line_judges_nothing() {
        let (base, writes) = mock_jobs(vec![request(
            CHECK,
            "publish-drift",
            &[],
            "not yet: checkout at cb053ed6, main at 9a1b2c3d\n",
        )])
        .await;
        let h = judge(base);
        h.invoke(&args(), &ctx()).await.unwrap();
        assert!(writes.lock().unwrap().is_empty());
    }

    /// At-least-once delivery: the second delivery of one close finds
    /// the check already judged and writes nothing more — and even with
    /// the note lost, an open follow-on carrying `for_check` is found
    /// before a twin is filed.
    #[tokio::test]
    async fn a_redelivery_files_one_follow_on() {
        let (base, writes) =
            mock_jobs(vec![request(CHECK, "publish-drift", &[], CLEAN_OUTPUT)]).await;
        let h = judge(base.clone());
        h.invoke(&args(), &ctx()).await.unwrap();
        h.invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(
            posts(&w).len(),
            1,
            "one follow-on across two deliveries: {w:?}"
        );
        assert_eq!(w.len(), 2, "and no second note: {w:?}");

        // The note never landed (the PATCH failed after the POST), but
        // the follow-on is open and linked: the redelivery files no
        // twin and writes the note it still owes.
        let mut check = request(CHECK, "publish-drift", &[], CLEAN_OUTPUT);
        let mut filed = request(
            "f0000000-0000-0000-0000-00000000000f",
            "publish-drift",
            &["--for-real"],
            "",
        );
        filed["status"] = json!("open");
        filed["metadata"][FOR_CHECK] = json!(CHECK);
        check["metadata"].as_object_mut().unwrap().remove(JUDGED);
        let (base, writes) = mock_jobs(vec![check, filed]).await;
        let h = judge(base);
        h.invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert!(posts(&w).is_empty(), "no twin: {w:?}");
        assert_eq!(w.len(), 1, "the owed note: {w:?}");
        assert_eq!(w[0].2["filed"], "f0000000-0000-0000-0000-00000000000f");
    }

    /// A rule judges one verb. Another verb's answer is not its
    /// business, and neither is the packet the rule itself files — the
    /// follow-on's own answer (`published N`) closes `answered` through
    /// the same event and must not chain.
    #[tokio::test]
    async fn another_verb_and_the_rules_own_follow_on_are_not_judged() {
        let (base, writes) =
            mock_jobs(vec![request(CHECK, "disk-report", &[], CLEAN_OUTPUT)]).await;
        let h = judge(base);
        h.invoke(&args(), &ctx()).await.unwrap();
        assert!(writes.lock().unwrap().is_empty(), "wrong verb");

        let (base, writes) = mock_jobs(vec![request(
            CHECK,
            "publish-drift",
            &["--for-real"],
            FOR_REAL_OUTPUT,
        )])
        .await;
        let h = judge(base);
        h.invoke(&args(), &ctx()).await.unwrap();
        assert!(
            writes.lock().unwrap().is_empty(),
            "the rule's own follow-on"
        );
    }

    #[test]
    fn the_verdict_is_the_last_matching_line_and_its_groups_are_numbers() {
        let re = regex::Regex::new(PATTERN).unwrap();
        let (line, groups) = verdict_groups(&re, CLEAN_OUTPUT).expect("matches");
        assert!(line.starts_with("publish-drift: would publish 3, skipped 20 equal, refused 0 ("));
        assert_eq!(groups, json!({"n": 3, "m": 20, "k": 0}));
        // The ops-runner merges streams and may append a marker after
        // the verdict: the LAST matching line wins, trailing text does
        // not hide it.
        let two = "publish-drift: would publish 1, skipped 1 equal, refused 1\npublish-drift: would publish 2, skipped 2 equal, refused 0\n[ops-runner: output truncated]\n";
        assert_eq!(verdict_groups(&re, two).unwrap().1["n"], 2);
        assert!(verdict_groups(&re, "no verdict here\n").is_none());
        assert!(
            verdict_groups(&re, FOR_REAL_OUTPUT).is_none(),
            "`published N` is not `would publish N`"
        );
    }

    /// THE MEASUREMENT the module doc claims: boss-expr evaluates the
    /// rule's `when` over the groups as JSON — one predicate language.
    #[test]
    fn when_is_boss_expr_over_the_groups() {
        let when = expr::parse(WHEN).unwrap();
        assert!(when_holds(&when, WHEN, &json!({"n": 3, "m": 20, "k": 0})).unwrap());
        assert!(!when_holds(&when, WHEN, &json!({"n": 3, "m": 19, "k": 1})).unwrap());
        assert!(!when_holds(&when, WHEN, &json!({"n": 0, "m": 23, "k": 0})).unwrap());
        // A group the pattern did not capture is absent, which is false
        // in comparison — not an error that pins the rule.
        assert!(!when_holds(&when, WHEN, &json!({"n": 3})).unwrap());
        // A predicate that is not a predicate is rule authoring.
        let bare = expr::parse("n").unwrap();
        assert!(matches!(
            when_holds(&bare, "n", &json!({"n": 3})),
            Err(HandlerError::Permanent(_))
        ));
    }

    /// A bad regex or a bad predicate is a permanent error naming the
    /// arg; a missing arg is a missing arg.
    #[tokio::test]
    async fn bad_rule_args_are_permanent() {
        let h = judge("http://unused".to_string());
        let err = h.invoke(&[], &ctx()).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::MissingArg(ref a) if a == "verb"),
            "{err:?}"
        );

        let mut a = args();
        a.iter_mut()
            .find(|(k, _)| k == "verdict_pattern")
            .unwrap()
            .1 = Value::String("(?P<n>[0-9]+".into());
        let err = h.invoke(&a, &ctx()).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("verdict_pattern")),
            "{err:?}"
        );

        let mut a = args();
        a.iter_mut().find(|(k, _)| k == "when").unwrap().1 = Value::String("k = = 0".into());
        let err = h.invoke(&a, &ctx()).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("when")),
            "{err:?}"
        );
        assert!(err.is_permanent());
    }

    #[test]
    fn then_args_split_on_whitespace_into_the_runners_array() {
        let mut a = args();
        a.iter_mut().find(|(k, _)| k == "then_args").unwrap().1 =
            Value::String("  --for-real   forge ".into());
        let j = Judgement::from_args(&a).unwrap();
        assert_eq!(j.then.unwrap().args, vec!["--for-real", "forge"]);
        let mut a = args();
        a.iter_mut().find(|(k, _)| k == "then_args").unwrap().1 = Value::String("".into());
        assert!(
            Judgement::from_args(&a)
                .unwrap()
                .then
                .unwrap()
                .args
                .is_empty()
        );
    }

    /// A verb that FAILED left a partial measurement, and the whole
    /// point of the `--check` half is that its answer is trusted
    /// enough to spend a `--for-real` on. Here the failed check's
    /// output still CARRIES the clean verdict line — a run that died
    /// after printing its table, or was killed at the timeout with the
    /// table already merged into the same field — so `when` would hold
    /// and the old handler would have published. Nothing is filed but
    /// the alarm, and the check says why on its own face.
    #[tokio::test]
    async fn a_failed_check_publishes_nothing_and_files_the_alarm_instead() {
        let (base, writes) = mock_jobs(vec![request_exit(
            CHECK,
            "publish-drift",
            &[],
            CLEAN_OUTPUT,
            "1",
        )])
        .await;
        let h = judge(base);
        h.invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(filed.len(), 1, "one alarm, nothing else filed: {w:?}");
        assert_eq!(
            filed[0]["kind"], "backlog-item",
            "a --for-real must not ride a measurement that did not finish: {w:?}"
        );
        assert_eq!(filed[0]["priority"], "urgent");
        let m = &filed[0]["metadata"];
        assert_eq!(m[FOR_REQUEST], CHECK, "the dedup key names the check");
        assert_eq!(m["exit"], "1");
        assert!(
            m["failed"].as_str().unwrap().contains("would publish 3"),
            "the verb's last line, verbatim: {m}"
        );
        // And the judged check carries the refusal, so a reader who
        // opens it is not left to re-derive why nothing happened.
        let note = w
            .iter()
            .find(|(me, p, _)| me == "PATCH" && p == &format!("/api/jobs/{CHECK}/metadata"))
            .map(|(_, _, b)| b.clone())
            .expect("the check is annotated");
        let judged = note[JUDGED].as_str().unwrap();
        assert!(judged.contains("exit 1"), "{judged}");
        assert!(
            !judged.contains("filed publish-drift"),
            "nothing was published: {judged}"
        );
    }

    /// EX_TEMPFAIL is not a failure. `publish-drift` on a checkout
    /// behind the newest converged train prints `not yet: …` and exits
    /// 75 having compared nothing — which happens on most converges,
    /// so treating it as a failure would file an urgent packet a day.
    /// It takes the path it always took: no verdict line, nothing
    /// judged, nothing filed, and no alarm.
    #[tokio::test]
    async fn a_not_yet_check_is_not_a_failure_and_files_nothing() {
        let out = "publish-drift: not yet: checkout at cb053ed6, main at 4cb3d3a7 — nothing compared, nothing published\n";
        let (base, writes) =
            mock_jobs(vec![request_exit(CHECK, "publish-drift", &[], out, "75")]).await;
        let h = judge(base);
        h.invoke(&args(), &ctx()).await.unwrap();
        assert!(
            writes.lock().unwrap().is_empty(),
            "a not-yet claims nothing: {:?}",
            writes.lock().unwrap()
        );
    }

    /// Idempotent under at-least-once delivery: the second delivery
    /// reads `judged` and stops, and even a delivery that lost the note
    /// finds the open alarm by `for_request` rather than filing a twin.
    #[tokio::test]
    async fn a_redelivered_failed_check_files_one_alarm() {
        let (base, writes) = mock_jobs(vec![request_exit(
            CHECK,
            "publish-drift",
            &[],
            CLEAN_OUTPUT,
            "124",
        )])
        .await;
        let h = judge(base.clone());
        h.invoke(&args(), &ctx()).await.unwrap();
        h.invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(filed.len(), 1, "one alarm across two deliveries: {w:?}");
        assert_eq!(filed[0]["kind"], "backlog-item", "{w:?}");
    }

    // -----------------------------------------------------------------
    // WATCH MODE (backlog 8d77d670, review H1): a rule with no follow-on
    // watches an UNATTENDED verb, so a run that did not do its job is an
    // alarm rather than a closed packet nobody reads.
    // -----------------------------------------------------------------

    const WATCHED: &str = "prune-registry-versions-daily";
    /// The prune's closing line, as prune-registry-versions.sh prints it.
    const WATCH_PATTERN: &str = "OK .* deleted (?P<deleted>[0-9]+) of (?P<planned>[0-9]+) planned";
    const PRUNE_RECORD: &str =
        r#"{"verb":"prune-registry-versions","dry_run":false,"planned":8,"deleted":8}"#;

    fn watch_args() -> Vec<(String, Value)> {
        [
            ("verb", WATCHED),
            ("verdict_pattern", WATCH_PATTERN),
            ("when", "deleted = planned"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.into())))
        .collect()
    }

    fn alarms(w: &[(String, String, serde_json::Value)]) -> Vec<serde_json::Value> {
        posts(w)
            .into_iter()
            .filter(|b| b["kind"] == "backlog-item")
            .collect()
    }

    fn note_on(w: &[(String, String, serde_json::Value)], id: &str) -> serde_json::Value {
        w.iter()
            .find(|(me, p, _)| me == "PATCH" && p == &format!("/api/jobs/{id}/metadata"))
            .map(|(_, _, b)| b.clone())
            .unwrap_or_else(|| panic!("{id} was not annotated: {w:?}"))
    }

    #[test]
    fn a_rule_without_a_follow_on_is_a_watch_and_half_a_follow_on_is_refused() {
        let j = Judgement::from_args(&watch_args()).unwrap();
        assert!(j.then.is_none(), "no then_* args means a watch");
        let mut half = watch_args();
        half.push(("then_verb".into(), Value::String("reclaim-disk".into())));
        let err = Judgement::from_args(&half).err().expect("half a follow-on");
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("then_host")),
            "{err:?}"
        );
    }

    /// The prune REFUSED (exit 2 — an underivable keep half, a ceiling,
    /// a 403) or failed part-way (exit 1). The request closed `answered`
    /// because the verb RAN, so nothing else says so: the watch files one
    /// urgent alarm naming the verb, the exit, its verdict line and the
    /// head of its output, where the record sits.
    #[tokio::test]
    async fn a_watched_verb_that_failed_files_its_alarm_naming_the_verb_and_output() {
        let out = format!(
            "{PRUNE_RECORD}\nprune-registry-versions: REFUSED — the plan deletes 1600 version(s), beyond the ceiling of 1500 per run\nprune-registry-versions:   Nothing was deleted.\n"
        );
        let (base, writes) = mock_jobs(vec![request_exit(CHECK, WATCHED, &[], &out, "2")]).await;
        judge(base).invoke(&watch_args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(filed.len(), 1, "one alarm and nothing else: {w:?}");
        let alarm = &filed[0];
        assert_eq!(alarm["kind"], "backlog-item");
        assert_eq!(alarm["priority"], "urgent");
        let title = alarm["title"].as_str().unwrap();
        assert!(
            title.contains(WATCHED) && title.contains("exit 2"),
            "{title}"
        );
        assert!(
            !title.contains("was not filed"),
            "a watch files no follow-on, so its alarm must not say one was withheld: {title}"
        );
        let m = &alarm["metadata"];
        assert_eq!(m[FOR_REQUEST], CHECK);
        assert_eq!(m["verb"], WATCHED);
        assert_eq!(m["exit"], "2");
        assert!(m["failed"].as_str().unwrap().contains("REFUSED"), "{m}");
        assert!(
            m["output_head"].as_str().unwrap().contains(PRUNE_RECORD),
            "the alarm carries the head of the output, where the record is: {m}"
        );
        let note = note_on(&w, CHECK);
        assert!(note[JUDGED].as_str().unwrap().contains("exit 2"), "{note}");
        assert!(note["alert"].is_string(), "{note}");
    }

    #[tokio::test]
    async fn a_redelivered_failed_watch_files_one_alarm() {
        let (base, writes) = mock_jobs(vec![request_exit(
            CHECK,
            WATCHED,
            &[],
            "prune-registry-versions: FAILED — DELETE boss:deadbee answered 500, not 204; stopping here:\n",
            "1",
        )])
        .await;
        let h = judge(base);
        h.invoke(&watch_args(), &ctx()).await.unwrap();
        h.invoke(&watch_args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(
            alarms(&w).len(),
            1,
            "one alarm across two deliveries: {w:?}"
        );
    }

    #[tokio::test]
    async fn a_watched_verb_that_answered_clean_files_nothing_and_says_so() {
        let out = format!(
            "{PRUNE_RECORD}\nprune-registry-versions: OK — deleted 8 of 8 planned version(s) across boss boss-ci; df\n"
        );
        let (base, writes) = mock_jobs(vec![request(CHECK, WATCHED, &[], &out)]).await;
        judge(base).invoke(&watch_args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert!(
            posts(&w).is_empty(),
            "a clean watched run files nothing: {w:?}"
        );
        let note = note_on(&w, CHECK);
        let judged = note[JUDGED].as_str().unwrap();
        assert!(judged.contains("deleted = planned"), "{judged}");
        assert!(judged.contains("holds"), "{judged}");
    }

    /// No evidence is not a pass: an exit 0 with no verdict line, or a
    /// verdict its predicate refuses, is an alarm for a watch — nothing
    /// else will read that run.
    #[tokio::test]
    async fn a_watched_verb_without_its_verdict_or_failing_it_is_an_alarm() {
        for (out, why) in [
            (
                "prune-registry-versions: list: every line follows\n",
                "no verdict",
            ),
            (
                "prune-registry-versions: OK — deleted 3 of 8 planned version(s) across boss boss-ci\n",
                "deleted = planned",
            ),
        ] {
            let (base, writes) = mock_jobs(vec![request(CHECK, WATCHED, &[], out)]).await;
            judge(base).invoke(&watch_args(), &ctx()).await.unwrap();
            let w = writes.lock().unwrap().clone();
            let filed = alarms(&w);
            assert_eq!(filed.len(), 1, "{why}: {w:?}");
            let detail = filed[0]["metadata"]["detail"].as_str().unwrap();
            assert!(detail.contains(why), "{why}: {detail}");
        }
    }

    /// A RUNNER refusal ran nothing, carries no exit, and closes the
    /// request `refused` — which a chain has always ignored and a watch
    /// must not: a daily verb the runner will not run is a daily bound
    /// that is not holding.
    #[tokio::test]
    async fn a_runner_refusal_of_a_watched_verb_is_an_alarm() {
        let refused = json!({
            "id": CHECK,
            "kind": "ops-request",
            "status": "closed",
            "metadata": { "host": "forge", "verb": WATCHED, "outcome": "refused" },
            "steps": [
                { "id": "r-execute", "spec_slug": "execute", "status": "completed",
                  "metadata": { "disposition": "refused",
                                "reason": "verb prune-registry-versions-daily is not in the allowlist (infra/ops/verbs/)" } },
            ],
        });
        let (base, writes) = mock_jobs(vec![refused]).await;
        let mut c = ctx();
        c.event_payload["outcome"] = json!("refused");
        judge(base.clone()).invoke(&watch_args(), &c).await.unwrap();
        let w = writes.lock().unwrap().clone();
        let filed = alarms(&w);
        assert_eq!(filed.len(), 1, "{w:?}");
        assert!(
            filed[0]["metadata"]["failed"]
                .as_str()
                .unwrap()
                .contains("not in the allowlist"),
            "the runner's reason, verbatim: {}",
            filed[0]
        );
        // A CHAIN still ignores a runner refusal, as it always has.
        let chained = json!({
            "id": CONVERGE, "kind": "ops-request", "status": "closed",
            "metadata": { "host": "boss-gcp", "verb": "publish-drift", "outcome": "refused" },
            "steps": [],
        });
        let (base, writes) = mock_jobs(vec![chained]).await;
        let mut c = ctx();
        c.event_payload["id"] = json!(CONVERGE);
        c.event_payload["outcome"] = json!("refused");
        judge(base).invoke(&args(), &c).await.unwrap();
        assert!(writes.lock().unwrap().is_empty());
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = judge("http://unused".to_string());
        assert_eq!(h.name(), "ops.judge");
        let emits = crate::cascade::handler_emits()
            .get("ops.judge")
            .cloned()
            .expect("the cascade table knows this handler");
        for topic in ["jobs.job.created", "jobs.job.updated"] {
            assert!(emits.contains(&topic), "emits {topic}: {emits:?}");
        }
    }
}
