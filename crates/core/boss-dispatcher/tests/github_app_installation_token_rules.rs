//! The GitHub App installation-token credentials are declared once and
//! fired twice (design 76155676, backlog 81eb6d4d).
//!
//! `credential.rotate.github-app-installation` runs off two rules per
//! credential: the EXPOSURE rule on a rotate-a-credential packet's scope
//! step, and the ROUTINE clock rule that re-mints before the hour an
//! installation token lives runs out. Both hand the handler the
//! credential's consumer declaration — which Secret, which narrowing,
//! which repository proves it — so that declaration lives in two files,
//! and CLAUDE.md §9a asks for the equality test. A refresh writing a
//! different Secret, or minting a wider token, than the rotation would be
//! two credentials under one registry id.
//!
//! And the clock rule's shape is data the handler cannot check: it must
//! be sub-hourly (an hourly firing cannot keep an hour-long token alive
//! against a window measured in minutes), it must say `phase = "refresh"`
//! (a clock firing without it is refused), and its window must be wider
//! than its cadence, or a token could expire between two looks.
//!
//! A SECOND SHAPE, MINTED PER ACT (design 76c46869, David 2026-09-28;
//! backlog bf8726c9). The org-admin token the GitHub verbs act with
//! (`infra/forge/github-act.sh`) is deliberately NOT kept alive: no clock
//! re-mints it, so between acts no admin token that has not expired
//! exists. Its two rules fire on the REQUEST instead — one when a request
//! naming a GitHub verb is filed (the plan half loads the token too), one
//! when that request's approve step records `approved` — and both run the
//! handler's refresh path, which mints only when the Secret's token is
//! absent or inside the window. That pair has no clock twin, so it is
//! declared here as its own class rather than by deleting the pair rule
//! above: the (filing, approval) pair carries ONE declaration word for
//! word, narrows to no repository (a repository being created cannot be
//! named in advance), and fires for exactly the verbs that run
//! github-act.sh — a verb added to that script's roster and missing here
//! would act with a token nobody minted.

use std::collections::BTreeSet;

use boss_core::calendar::Cadence;
use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{
    RawDoStep, RawRegistry, RawRule, Registry, match_event, parse_raw_path,
};
use boss_testing::{dispatcher_rules_dir, repo_root};
use serde_json::json;

const HANDLER: &str = "credential.rotate.github-app-installation";

/// (exposure rule, routine clock rule), per credential.
const PAIRS: &[(&str, &str)] = &[(
    "broker-rotates-the-github-dr-push-token",
    "broker-refreshes-the-github-dr-push-token",
)];

/// (filing rule, approval rule), per credential minted per act.
const PER_ACT: &[(&str, &str)] = &[(
    "broker-mints-the-algedonic-dev-admin-token-when-a-github-request-is-filed",
    "broker-re-mints-the-algedonic-dev-admin-token-when-a-github-request-is-approved",
)];

/// The script every per-act GitHub verb runs (its `argv[0]` in
/// `infra/ops/verbs/<verb>.json`) — the roster the filing rule must match.
const GITHUB_ACT: &str = "infra/forge/github-act.sh";

/// The widest window the handler accepts (`Declaration::parse` in
/// credential_rotate_github_app.rs refuses anything outside 1..=59, since
/// an installation token lives 60). The filing mint uses it: the act
/// before this one revoked the token the Secret holds, and a verb that
/// died before marking it expired left it under a live expiry, so the
/// filing must mint unless the held token is under a minute old.
const WIDEST_WINDOW: u32 = 59;

/// The declaration both rules carry.
const DECLARATION: &[&str] = &[
    "secret_namespace",
    "secret_name",
    "secret_key",
    "credential_id",
    "verify_repo",
    "repositories",
    "permissions",
];

fn rules() -> RawRegistry {
    parse_raw_path(dispatcher_rules_dir()).expect("parse the rule directory")
}

fn rule<'a>(reg: &'a RawRegistry, name: &str) -> &'a RawRule {
    reg.rules
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("rule {name} is in the authored directory"))
}

#[test]
fn every_installation_token_rule_has_its_twin() {
    let reg = rules();
    let firing: Vec<&str> = reg
        .rules
        .iter()
        .filter(|r| r.do_steps.iter().any(|d| d.handler == HANDLER))
        .map(|r| r.name.as_str())
        .collect();
    for name in &firing {
        assert!(
            PAIRS
                .iter()
                .chain(PER_ACT)
                .any(|(a, b)| a == name || b == name),
            "{name} runs {HANDLER} but is in no (rotation, refresh) pair and no per-act \
             (filing, approval) pair here: a credential with an exposure path and no refresh \
             dies within the hour, one with a refresh and no exposure path cannot be rotated \
             on a leak, and a per-act mint fires on both halves of the request it serves"
        );
    }
    for (a, b) in PAIRS.iter().chain(PER_ACT) {
        assert!(firing.contains(a), "{a} runs {HANDLER}");
        assert!(firing.contains(b), "{b} runs {HANDLER}");
    }
}

// ---------------------------------------------------------------------------
// The per-act class (design 76c46869)
// ---------------------------------------------------------------------------

fn window(rule: &RawRule) -> u32 {
    rule.do_steps[0]
        .args
        .get("refresh_within_minutes")
        .map(|v| v.trim_matches('"'))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("{} names refresh_within_minutes", rule.name))
}

#[test]
fn the_approval_mint_carries_the_filing_mints_declaration_verbatim() {
    let reg = rules();
    for (filing, approval) in PER_ACT {
        let (f, a) = (rule(&reg, filing), rule(&reg, approval));
        assert_eq!((f.do_steps.len(), a.do_steps.len()), (1, 1));
        let (fs, as_) = (&f.do_steps[0], &a.do_steps[0]);
        assert_eq!(
            (fs.handler.as_str(), as_.handler.as_str()),
            (HANDLER, HANDLER)
        );
        for key in DECLARATION.iter().filter(|k| **k != "repositories") {
            assert!(fs.args.contains_key(*key), "{filing} declares `{key}`");
            assert_eq!(
                as_.args.get(*key),
                fs.args.get(*key),
                "{approval} and {filing} disagree on `{key}`: two credentials under one \
                 registry id"
            );
        }
        assert_eq!(
            as_.args.get("installation_id"),
            fs.args.get("installation_id"),
            "{approval} and {filing} disagree on `installation_id`"
        );
        for (name, step) in [(filing, fs), (approval, as_)] {
            assert!(
                !step.args.contains_key("repositories"),
                "{name} narrows to repositories: a per-act admin token serves an act on a \
                 repository that may not exist yet (create-repository), and GitHub refuses \
                 to narrow a token to a repository it cannot see"
            );
            assert_eq!(
                step.args.get("phase").map(String::as_str),
                Some("\"refresh\""),
                "{name}: a per-act mint runs the handler's refresh path, which mints only \
                 when the Secret's token is absent or inside the window, and needs no packet"
            );
        }
    }
}

#[test]
fn a_per_act_mint_fires_on_the_request_and_never_on_a_clock() {
    let reg = rules();
    for (filing, approval) in PER_ACT {
        let (f, a) = (rule(&reg, filing), rule(&reg, approval));
        for r in [f, a] {
            assert!(
                r.schedule.is_none(),
                "{}: a per-act token has no clock — a clock would keep an org-admin \
                 credential standing between acts, which design 76c46869 Q1 refused",
                r.name
            );
        }
        assert_eq!(f.on_event.as_deref(), Some("jobs.job.created"), "{filing}");
        assert_eq!(
            a.on_event.as_deref(),
            Some("step.done.sign-off"),
            "{approval}"
        );
        assert_eq!(
            window(f),
            WIDEST_WINDOW,
            "{filing}: the filing mint must mint for any token older than a minute — the \
             previous act revoked the one the Secret holds, and a verb that died before \
             marking it expired left its recorded expiry live"
        );
        let w = window(a);
        assert!(
            (1..WIDEST_WINDOW).contains(&w),
            "{approval}: a {w}-minute window re-mints on approval only when the filing's \
             token has aged into it, and must be narrower than the filing's"
        );
    }
}

/// The organisation a per-act credential serves, read off its declaration:
/// the credential and its Secret are named after the owner the verbs
/// select a slot by, `github-app-<owner>` (design 76c46869, "named after
/// the owner, not the use"), and github-act.sh finds its rule by that id.
fn owner_of(step: &RawDoStep) -> String {
    let id = step
        .args
        .get("credential_id")
        .map(|v| v.trim_matches('"').to_string())
        .expect("a per-act rule declares credential_id");
    id.strip_prefix("github-app-")
        .unwrap_or_else(|| panic!("{id} is not named github-app-<owner>"))
        .to_string()
}

/// `payload` with its request's arguments replaced — `None` removes them.
fn naming(mut payload: serde_json::Value, args: Option<serde_json::Value>) -> serde_json::Value {
    let md = payload["metadata"].as_object_mut().expect("metadata");
    match args {
        Some(a) => md.insert("args".into(), a),
        None => md.remove("args"),
    };
    payload
}

/// A request naming ANOTHER owner must not mint this organisation's admin
/// token (review of car 0660454c, 2026-09-28: `github-create-repository
/// dauld …` minted an algedonic-dev admin token that lived its hour
/// unused, since the verb reads dauld's slot). The owner is the request's
/// first argument in all six GitHub verbs (their argv in infra/ops/verbs/),
/// and it is compared exactly, as github-act.sh selects the slot.
#[test]
fn a_per_act_mint_fires_only_for_a_request_naming_its_organisation() {
    let raw = rules();
    let reg = Registry::from_raw(rules()).expect("the shipped rules parse together");
    let github: Vec<(String, bool)> = verbs()
        .into_iter()
        .filter(|(_, g, _)| *g)
        .map(|(v, _, a)| (v, a))
        .collect();
    assert!(!github.is_empty(), "control: GitHub verbs exist");
    for (filing, approval) in PER_ACT {
        let owner = owner_of(&rule(&raw, filing).do_steps[0]);
        assert_eq!(owner, owner_of(&rule(&raw, approval).do_steps[0]));
        let other = json!(["dauld", "boss-dr", "private"]);
        let recased = json!([owner.to_uppercase(), "boss-dr", "private"]);
        let ours = json!([owner, "boss-dr", "private"]);
        for (verb, needs_approval) in &github {
            let f = |args| fired(&reg, "jobs.job.created", &naming(filed(verb), args));
            assert!(
                f(Some(ours.clone())).contains(*filing),
                "control: {filing} fires on a `{verb}` request naming {owner}"
            );
            for (why, args) in [
                ("another owner", Some(other.clone())),
                ("the owner in another case", Some(recased.clone())),
                ("no arguments", Some(json!([]))),
                ("no args key", None),
            ] {
                assert!(
                    !f(args).contains(*filing),
                    "{filing} minted an {owner} admin token for a `{verb}` request naming \
                     {why}"
                );
            }
            if !needs_approval {
                continue;
            }
            let a = |args| {
                fired(
                    &reg,
                    "step.done.sign-off",
                    &naming(approved(verb, "approved"), args),
                )
            };
            assert!(
                a(Some(ours.clone())).contains(*approval),
                "control: {approval} fires on an approved `{verb}` naming {owner}"
            );
            for (why, args) in [
                ("another owner", Some(other.clone())),
                ("no arguments", Some(json!([]))),
                ("no args key", None),
            ] {
                assert!(
                    !a(args).contains(*approval),
                    "{approval} re-minted an {owner} admin token for an approved `{verb}` \
                     naming {why}"
                );
            }
        }
    }
}

/// THE TOKEN IS THE REQUEST'S OWN (backlog 4ce4ec55; the review of car
/// 57a2c56e, finding 1). With one key per owner, two writes approved
/// before the first ran shared one token: approval 2 found approval 1's
/// fresh and minted nothing, write 1 revoked it, and write 2 was stranded
/// with a spent approval. So both rules name the ops-request they fire for
/// — the filing's `id` (the serialized Job), the approval's `job_id` (the
/// request whose approve step completed) — and the handler keys the
/// Secret `<secret_key>-<request id>`; github-act.sh renders and revokes
/// that key alone, by the `OPS_REQUEST_ID` the runner hands it. Bound
/// through the real matcher, so a rule naming the wrong field — the
/// approve step's own id, say — keys a token no verb will ever read.
#[test]
fn a_per_act_mint_keys_its_token_on_the_request_it_fires_for() {
    const FILED: &str = "aaaaaaaa-0000-4000-8000-00000000000f";
    const APPROVED: &str = "bbbbbbbb-0000-4000-8000-00000000000a";
    let reg = Registry::from_raw(rules()).expect("the shipped rules parse together");
    let request_of = |topic: &str, payload: &serde_json::Value, rule: &str| {
        match_event(&reg, topic, payload, &NoHelpers)
            .matched
            .into_iter()
            .find(|m| m.rule_name == rule)
            .unwrap_or_else(|| panic!("control: {rule} fires on {topic} {payload}"))
            .invocations[0]
            .args
            .iter()
            .find(|(k, _)| k == "request_id")
            .map(|(_, v)| v.clone())
    };
    for (filing, approval) in PER_ACT {
        let mut f = filed("github-create-repository");
        f["id"] = json!(FILED);
        assert_eq!(
            request_of("jobs.job.created", &f, filing),
            Some(Value::String(FILED.into())),
            "{filing} keys the token it mints on the request being filed"
        );
        let mut a = approved("github-create-repository", "approved");
        a["job_id"] = json!(APPROVED);
        assert_eq!(
            request_of("step.done.sign-off", &a, approval),
            Some(Value::String(APPROVED.into())),
            "{approval} keys the token it re-mints on the request whose plan was approved — \
             the key the filing minted into, and the one that request's verbs render"
        );
    }
}

/// Every verb in `infra/ops/verbs/`, with whether it runs github-act.sh
/// and whether it waits for a passkey approval.
fn verbs() -> Vec<(String, bool, bool)> {
    let dir = repo_root().join("infra/ops/verbs");
    let mut out: Vec<(String, bool, bool)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let v: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())),
            )
            .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            let name = p.file_stem().expect("stem").to_string_lossy().to_string();
            let github = v["argv"][0].as_str() == Some(GITHUB_ACT);
            let approval = v["requires_approval"].as_bool().unwrap_or(false);
            (name, github, approval)
        })
        .collect();
    out.sort();
    out
}

fn fired(reg: &Registry, topic: &str, payload: &serde_json::Value) -> BTreeSet<String> {
    let outcome = match_event(reg, topic, payload, &NoHelpers);
    let ours: Vec<&str> = PER_ACT.iter().flat_map(|(a, b)| [*a, *b]).collect();
    for s in &outcome.skipped {
        assert!(
            !ours.contains(&s.rule.as_str()),
            "{} could not be judged on {topic} {payload}: {:?} — a predicate that raises \
             is redelivered to a dead letter, not a quiet no",
            s.rule,
            s.err
        );
    }
    outcome
        .matched
        .into_iter()
        .filter(|m| ours.contains(&m.rule_name.as_str()))
        .inspect(|m| {
            assert!(m.invocations.iter().all(|i| i.handler == HANDLER));
            let phase = m.invocations[0]
                .args
                .iter()
                .find(|(k, _)| k == "phase")
                .map(|(_, v)| v.clone());
            assert_eq!(phase, Some(Value::String("refresh".into())));
        })
        .map(|m| m.rule_name)
        .collect()
}

/// `jobs.job.created` carries the serialized Job (jobs.rs, JOB_CREATED).
fn filed(verb: &str) -> serde_json::Value {
    json!({
        "id": "11111111-1111-1111-1111-111111111111",
        "kind": "ops-request",
        "subject": {"kind": "custom", "id": "forge"},
        "title": format!("{verb} on forge"),
        "status": "open",
        "metadata": {"host": "forge", "verb": verb, "args": ["algedonic-dev", "boss-dr", "private"]},
    })
}

/// `step.done.sign-off` as steps.rs emits it, over the approve step as
/// the ops runner leaves it: the plan written with the WRITE verb it was
/// rendered for (render_plan in infra/ops/ops-runner.sh), and the
/// decision the passkey signed.
fn approved(verb: &str, decision: &str) -> serde_json::Value {
    json!({
        "job_id": "11111111-1111-1111-1111-111111111111",
        "step_id": "22222222-2222-2222-2222-222222222222",
        "kind": "sign-off",
        "subject_kind": "custom",
        "subject_id": "forge",
        "workflow_kind": "ops-request",
        "job_owner_id": "emp-david",
        "completed_on": "2026-09-28",
        "metadata": {
            "plan": "POST /orgs/algedonic-dev/repos",
            "verb": verb,
            "host": "forge",
            "args": ["algedonic-dev", "boss-dr", "private"],
            "rendered_plan_sha256": "0000",
            "decision": decision,
        },
        "notify_on_done": false,
        "spec_slug": "approve",
    })
}

#[test]
fn the_per_act_mints_fire_for_exactly_the_verbs_that_run_github_act() {
    let reg = Registry::from_raw(rules()).expect("the shipped rules parse together");
    let verbs = verbs();
    assert!(
        verbs.iter().filter(|(_, g, _)| *g).count() >= 2,
        "control: the verb directory holds the GitHub verbs this pin is about: {verbs:?}"
    );
    for (filing, approval) in PER_ACT {
        for (verb, github, needs_approval) in &verbs {
            let on_filing = fired(&reg, "jobs.job.created", &filed(verb));
            assert_eq!(
                on_filing.contains(*filing),
                *github,
                "{filing} on a filed `{verb}` request (runs {GITHUB_ACT}: {github}): a GitHub \
                 verb that finds no token refuses, and any other verb must not mint an \
                 org-admin token"
            );
            assert!(
                !on_filing.contains(*approval),
                "{approval} fired on a filing"
            );

            let on_approval = fired(&reg, "step.done.sign-off", &approved(verb, "approved"));
            assert_eq!(
                on_approval.contains(*approval),
                *github && *needs_approval,
                "{approval} on an approved `{verb}` plan (GitHub write: {})",
                *github && *needs_approval
            );
            assert!(
                !on_approval.contains(*filing),
                "{filing} fired on an approval"
            );

            let rejected = fired(&reg, "step.done.sign-off", &approved(verb, "rejected"));
            assert!(
                rejected.is_empty(),
                "a rejected `{verb}` plan minted: {rejected:?} — only an approval opens the \
                 act, so only an approval may mint for it"
            );
        }
        // Another workflow's step and another kind's job never mint.
        let mut other = filed("github-create-repository");
        other["kind"] = json!("backlog-item");
        assert!(fired(&reg, "jobs.job.created", &other).is_empty());
        let mut other = approved("github-create-repository", "approved");
        other["workflow_kind"] = json!("design-doc");
        assert!(fired(&reg, "step.done.sign-off", &other).is_empty());
    }
}

#[test]
fn the_refresh_carries_the_rotations_declaration_verbatim() {
    let reg = rules();
    for (rotation, refresh) in PAIRS {
        let (rot, re) = (
            &rule(&reg, rotation).do_steps[0],
            &rule(&reg, refresh).do_steps[0],
        );
        assert_eq!(
            (rot.handler.as_str(), re.handler.as_str()),
            (HANDLER, HANDLER)
        );
        for key in DECLARATION {
            assert!(rot.args.contains_key(*key), "{rotation} declares `{key}`");
            assert_eq!(
                re.args.get(*key),
                rot.args.get(*key),
                "{refresh} and {rotation} disagree on `{key}`: two credentials under one \
                 registry id"
            );
        }
        // Optional — absent is the root's own installation (algedonic-dev)
        // — but both, or neither: a refresh minting for another
        // organisation's installation is another credential.
        assert_eq!(
            re.args.get("installation_id"),
            rot.args.get("installation_id"),
            "{refresh} and {rotation} disagree on `installation_id`"
        );
        assert!(
            !rot.args.contains_key("phase"),
            "{rotation} is the packet door: it passes no phase"
        );
    }
}

#[test]
fn the_refresh_is_a_sub_hourly_clock_rule_whose_window_outlasts_its_cadence() {
    let reg = rules();
    for (_, refresh) in PAIRS {
        let r = rule(&reg, refresh);
        assert!(
            r.on_event.is_none(),
            "{refresh} is clock-driven: {:?}",
            r.on_event
        );
        let schedule = r
            .schedule
            .as_ref()
            .unwrap_or_else(|| panic!("{refresh} declares a schedule"));
        let Cadence::EveryNMinutes(every) = schedule.cadence else {
            panic!(
                "{refresh}: cadence {:?} is not sub-hourly — an installation token lives one \
                 hour",
                schedule.cadence
            );
        };
        let step = &r.do_steps[0];
        assert_eq!(
            step.args.get("phase").map(String::as_str),
            Some("\"refresh\""),
            "phase = \"refresh\" is the handler's contract for a clock firing"
        );
        let window: u32 = step
            .args
            .get("refresh_within_minutes")
            .map(|v| v.trim_matches('"'))
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("{refresh} names refresh_within_minutes"));
        assert!(
            window > every && window < 60,
            "{refresh}: a {window}-minute window looked at every {every} minutes — the window \
             must be wider than the cadence (or a token expires between two looks) and \
             narrower than the hour a token lives"
        );
    }
}
