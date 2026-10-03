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

/// Per-request mints with NO approval twin (backlog d2b7c947): the
/// publish verb's approval is David's passkey on the publish packet, which
/// completes BEFORE its ops-request is filed, so the filing is the only
/// moment to mint at. Each is judged by the `a_publish_*` pins below.
const PER_REQUEST_ONLY: &[&str] =
    &["broker-mints-the-algedonic-dev-publish-token-when-a-publish-request-is-filed"];

/// The script every per-act GitHub verb runs (its `argv[0]` in
/// `infra/ops/verbs/<verb>.json`) — the roster the filing rule must match.
const GITHUB_ACT: &str = "infra/forge/github-act.sh";

/// The verb the publish mint serves, and the script that renders its
/// token by naming the rule's file.
const PUBLISH_VERB: &str = "publish-github-pr";
const PUBLISH_SCRIPT: &str = "infra/forge/publish-github-pr.sh";
/// The merge of the PR the publish opened (backlog 602fe95f): the same
/// script with `--merge` fixed in its argv, minting under the same rule.
const MERGE_VERB: &str = "merge-publish-pr";

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
                .any(|(a, b)| a == name || b == name)
                || PER_REQUEST_ONLY.contains(name),
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
    for a in PER_REQUEST_ONLY {
        assert!(firing.contains(a), "{a} runs {HANDLER}");
    }
}

// ---------------------------------------------------------------------------
// The publish mint (backlog d2b7c947)
// ---------------------------------------------------------------------------

/// The publish rule's one do-step.
fn publish_step(reg: &RawRegistry) -> &RawDoStep {
    let r = rule(reg, PER_REQUEST_ONLY[0]);
    assert_eq!(r.do_steps.len(), 1, "{}", r.name);
    assert_eq!(r.do_steps[0].handler, HANDLER, "{}", r.name);
    &r.do_steps[0]
}

fn arg<'a>(step: &'a RawDoStep, key: &str) -> &'a str {
    step.args
        .get(key)
        .map(|v| v.trim_matches('"'))
        .unwrap_or_else(|| panic!("the publish mint declares `{key}`"))
}

/// The public mirror, as `infra/estate/estate.toml` declares it — the one
/// spelling (the_public_mirror_url_lives_once.rs) — as owner/repo.
fn declared_mirror_slug() -> String {
    let text = std::fs::read_to_string(repo_root().join("infra/estate/estate.toml"))
        .expect("read estate.toml");
    let url = text
        .lines()
        .find_map(|l| {
            l.strip_prefix("mirror_url = \"")
                .and_then(|r| r.strip_suffix('"'))
        })
        .expect("estate.toml declares mirror_url");
    url.split('/').skip(3).collect::<Vec<_>>().join("/")
}

/// Whether the publish mint fires on `topic` / `payload`, judged by the
/// real matcher — and never by a predicate that raised.
fn publish_fires(reg: &Registry, topic: &str, payload: &serde_json::Value) -> bool {
    let outcome = match_event(reg, topic, payload, &NoHelpers);
    assert!(
        !outcome
            .skipped
            .iter()
            .any(|s| s.rule == PER_REQUEST_ONLY[0]),
        "{} could not be judged on {topic} {payload}: a predicate that raises is redelivered \
         to a dead letter, not a quiet no",
        PER_REQUEST_ONLY[0]
    );
    outcome
        .matched
        .iter()
        .any(|m| m.rule_name == PER_REQUEST_ONLY[0])
}

/// It fires when a publish RUN or its MERGE (backlog 602fe95f) is filed
/// and on nothing else: not on the verb's `--check` (which holds no
/// token), not on any other verb, not on another kind's job and not on an
/// approval — and it keys the token on the request being filed, through
/// the refresh path, with no clock.
#[test]
fn a_publish_mint_fires_on_a_filed_publish_run_alone_and_keys_on_it() {
    const FILED: &str = "cccccccc-0000-4000-8000-00000000000f";
    let raw = rules();
    let r = rule(&raw, PER_REQUEST_ONLY[0]);
    assert!(r.schedule.is_none(), "a per-request token has no clock");
    assert_eq!(r.on_event.as_deref(), Some("jobs.job.created"));
    let step = publish_step(&raw);
    assert_eq!(arg(step, "phase"), "refresh");
    assert_eq!(
        arg(step, "refresh_within_minutes").parse::<u32>().ok(),
        Some(WIDEST_WINDOW),
        "the filing mints for any token older than a minute, as the admin filing does"
    );

    let reg = Registry::from_raw(rules()).expect("the shipped rules parse together");
    // The spawn rule files a publish run with no args at all.
    let run = naming(filed(PUBLISH_VERB), None);
    assert!(
        publish_fires(&reg, "jobs.job.created", &run),
        "control: the mint fires on a filed `{PUBLISH_VERB}` run"
    );
    assert!(
        publish_fires(
            &reg,
            "jobs.job.created",
            &naming(filed(PUBLISH_VERB), Some(json!([])))
        ),
        "a run filed with an empty args list is a run"
    );
    assert!(
        !publish_fires(
            &reg,
            "jobs.job.created",
            &naming(filed(PUBLISH_VERB), Some(json!(["--check"])))
        ),
        "`{PUBLISH_VERB} --check` holds no token and must not mint one"
    );
    // The merge (backlog 602fe95f): its rule files it with no args, and
    // it pushes the approved snapshot to main with this same token.
    assert!(
        publish_fires(&reg, "jobs.job.created", &naming(filed(MERGE_VERB), None)),
        "a filed `{MERGE_VERB}` request mints the token its push needs"
    );
    let merge_verb = repo_root().join(format!("infra/ops/verbs/{MERGE_VERB}.json"));
    let merge_argv: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&merge_verb).expect("the merge verb file"))
            .expect("the merge verb is JSON");
    assert_eq!(
        merge_argv["argv"],
        json!([PUBLISH_SCRIPT, "--merge"]),
        "the merge verb runs the publish script, whose token this rule declares"
    );
    for (verb, _, _) in verbs()
        .iter()
        .filter(|(v, _, _)| v != PUBLISH_VERB && v != MERGE_VERB)
    {
        assert!(
            !publish_fires(&reg, "jobs.job.created", &filed(verb)),
            "the publish mint fired on a filed `{verb}` request"
        );
    }
    let mut other = run.clone();
    other["kind"] = json!("backlog-item");
    assert!(!publish_fires(&reg, "jobs.job.created", &other));
    assert!(
        !publish_fires(
            &reg,
            "step.done.sign-off",
            &approved(PUBLISH_VERB, "approved")
        ),
        "the publish mint fires on the filing, never on an approval"
    );

    let mut keyed = run;
    keyed["id"] = json!(FILED);
    let request = match_event(&reg, "jobs.job.created", &keyed, &NoHelpers)
        .matched
        .into_iter()
        .find(|m| m.rule_name == PER_REQUEST_ONLY[0])
        .expect("control: it fires")
        .invocations[0]
        .args
        .iter()
        .find(|(k, _)| k == "request_id")
        .map(|(_, v)| v.clone());
    assert_eq!(
        request,
        Some(Value::String(FILED.into())),
        "the publish token is keyed on the request being filed — the OPS_REQUEST_ID the \
         verb renders by"
    );
}

/// NARROWER THAN THE ADMIN TOKEN, ON BOTH AXES. One repository — the
/// declared mirror, and nothing else — and exactly the four permissions a
/// publish uses; never administration. The same installation, Secret and
/// registry id as the admin mint, under a key namespace of its own, so
/// neither consumer renders the other's token.
#[test]
fn a_publish_token_is_narrowed_to_the_mirror_and_a_publishes_permissions() {
    let raw = rules();
    let step = publish_step(&raw);
    let slug = declared_mirror_slug();
    let name = slug.split('/').nth(1).expect("owner/repo");
    assert_eq!(
        arg(step, "verify_repo"),
        slug,
        "the publish token proves itself on the declared mirror"
    );
    assert_eq!(
        arg(step, "repositories"),
        name,
        "the publish token is narrowed to the declared mirror alone"
    );
    let permissions: BTreeSet<&str> = arg(step, "permissions").split(',').collect();
    assert_eq!(
        permissions,
        BTreeSet::from([
            "contents:write",
            "metadata:read",
            "pull_requests:write",
            "workflows:write"
        ]),
        "a publish pushes a branch carrying .github/workflows, opens and closes PRs, and \
         deletes closed branches — and nothing more"
    );

    let admin = &rule(&raw, PER_ACT[0].0).do_steps[0];
    for key in ["secret_namespace", "secret_name", "credential_id"] {
        assert_eq!(
            step.args.get(key),
            admin.args.get(key),
            "the publish mint and the admin filing mint disagree on `{key}`: one installation, \
             one Secret, one registry row"
        );
    }
    assert_ne!(
        step.args.get("secret_key"),
        admin.args.get("secret_key"),
        "the publish token must live under a key namespace of its own"
    );
    assert_eq!(
        step.args.get("installation_id"),
        admin.args.get("installation_id")
    );
    assert_eq!(owner_of(step), slug.split('/').next().unwrap());
}

/// github-act.sh picks its rule as the FIRST file in the directory that
/// declares `credential_id = github-app-<owner>` — which must stay the
/// admin filing rule, now that the publish mint declares the same id: a
/// GitHub act rendering the publish key would find nothing to act with.
/// And the publish verb names its rule's file, which must exist.
#[test]
fn github_act_still_selects_the_admin_rule_and_the_publish_verb_names_its_own() {
    let owner = owner_of(&rule(&rules(), PER_ACT[0].0).do_steps[0]);
    let needle = format!("credential_id = \"\\\"github-app-{owner}\\\"\"");
    let dir = dispatcher_rules_dir();
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .expect("read the rule directory")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    let first = files
        .iter()
        .find(|p| {
            std::fs::read_to_string(p)
                .unwrap_or_default()
                .contains(&needle)
        })
        .expect("a rule declares the installation's credential");
    assert_eq!(
        first.file_stem().and_then(|s| s.to_str()),
        Some(PER_ACT[0].0),
        "github-act.sh would select {} for github-app-{owner}",
        first.display()
    );

    let script =
        std::fs::read_to_string(repo_root().join(PUBLISH_SCRIPT)).expect("the publish verb");
    let file = format!("{}.toml", PER_REQUEST_ONLY[0]);
    assert!(
        script.contains(&format!("dispatcher/rules/{file}")),
        "{PUBLISH_SCRIPT} must render its token from {file}"
    );
    assert!(dir.join(&file).is_file(), "{file} is in the rule directory");
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

/// The GitHub push targets of `infra/forge/offsite-push.sh`: every
/// credential id its manifest names for a target that pushes branches.
fn pushing_credentials() -> BTreeSet<String> {
    let path = repo_root().join("infra/forge/offsite-push.json");
    let text = std::fs::read_to_string(&path).expect("read offsite-push.json");
    let manifest: serde_json::Value = serde_json::from_str(&text).expect("parse offsite-push.json");
    manifest["targets"]
        .as_array()
        .expect("offsite-push.json has a targets array")
        .iter()
        .filter(|t| t["branches"].as_array().is_some_and(|b| !b.is_empty()))
        .filter_map(|t| t["credential"].as_str().map(str::to_string))
        .collect()
}

/// Backlog d76cc3ac (2026-09-29): GitHub refused the DR push of forge main
/// to algedonic-dev/boss-dr — "refusing to allow a GitHub App to create or
/// update workflow .github/workflows/ci.yml without workflows permission" —
/// because the token was narrowed to contents:write,metadata:read. A token
/// that pushes THIS tree must carry workflows:write whenever the tree holds
/// a workflow file, or every forge converge exits 1 on the push.
#[test]
fn a_token_that_pushes_this_tree_may_write_its_workflows() {
    let workflows = repo_root().join(".github/workflows");
    let holds_workflows = std::fs::read_dir(&workflows)
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    if !holds_workflows {
        return;
    }
    let pushers = pushing_credentials();
    let reg = rules();
    let mut judged = 0;
    for r in reg
        .rules
        .iter()
        .filter(|r| r.do_steps.iter().any(|d| d.handler == HANDLER))
    {
        let step = &r.do_steps[0];
        let Some(id) = step.args.get("credential_id").map(|v| v.trim_matches('"')) else {
            continue;
        };
        if !pushers.contains(id) {
            continue;
        }
        judged += 1;
        let permissions = step
            .args
            .get("permissions")
            .map(|v| v.trim_matches('"'))
            .unwrap_or_default();
        assert!(
            permissions
                .split(',')
                .map(str::trim)
                .any(|p| p == "workflows:write"),
            "{}: credential {id} pushes this tree to GitHub (infra/forge/offsite-push.json), \
             and the tree holds .github/workflows, but its permissions {permissions:?} lack \
             workflows:write — GitHub refuses such a push (backlog d76cc3ac)",
            r.name
        );
    }
    assert!(
        judged > 0,
        "no {HANDLER} rule mints a credential offsite-push.json pushes with: {pushers:?}"
    );
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
