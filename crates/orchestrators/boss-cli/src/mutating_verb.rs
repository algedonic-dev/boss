//! A CAR WHOSE DIFF TOUCHES AN OPS VERB, WHAT A VERB RUNS, OR THE
//! RUNNER THAT RUNS IT WAITS AT THE DOCK FOR ITS REVIEW — read off the
//! diff, not off the item (backlog fdbb447e part 3, design 3036296f q2,
//! David 2026-09-27: "the gate reads the diff; a car that adds or
//! changes a MUTATING verb or its script cannot board until a review
//! verdict of RELEASE is recorded on it").
//!
//! WHY THE DIFF. The trust-boundary hold (backlog 486dde37,
//! `trust_boundary.rs`) keys on the ITEM's mark: a triager who measured
//! the change writes `metadata.trust_boundary`, and `boss gate` refuses
//! a park without `--hold`. An item nobody marked — a fix filed as a
//! typo, a car answering a different item, a partial car of a plan —
//! parked unheld and boarded on depth within minutes of its green, so a
//! change to what a host-mutating verb may do could reach main with no
//! review at all. On 2026-09-27 the adversarial review caught every
//! fail-open guard in the MUTATING verbs and the gate caught none; the
//! review is the check, and it only runs on a car that waits for it.
//!
//! THE ONE PREDICATE. [`judge`] reads a car's diff and the tree on BOTH
//! sides of it; [`Judgement::hold_reason`] is the one answer every door
//! reads (the `is_train_gate` pattern — one definition, several
//! readers):
//!
//! - `boss gate` with park intent: the car is filed HELD with this
//!   reason, appended to any `--hold` the gate was given (`gate.rs`).
//! - `boss rerail --finish`: the car is held BEFORE the fresh receipt
//!   makes it boardable (`rerail.rs`).
//! - `boss park`: refused, naming the two doors above (`park.rs`).
//!
//! The hold is the one the dock already honours (`metadata.hold` on the
//! car's review step, read by `stranded::hold_reason` in the conductor,
//! the loading-dock row and `boss orient`), and `boss release` clears it.
//!
//! WHAT IS COVERED, on each side of the change, over-approximated so it
//! fails closed (the adversarial review of this car's first head,
//! 0243fde5, found four ways past a narrower rule):
//!
//! 1. every file under [`OPS_DIR`] — every verb, whatever its label,
//!    and the approval runner (`ops-runner.sh`, `verbs-allowlist.sh`).
//!    NOT only the MUTATING ones: the label is the key AND the thing
//!    under review, so a read-only verb's script edited to mutate, or a
//!    new verb written without the word, would judge itself harmless.
//! 2. every `argv` entry of every verb that contains a `/` — so an
//!    interpreter at `argv[0]` (`["bash", "infra/forge/x.sh"]`) does not
//!    hide the script behind it.
//! 3. the transitive closure: any file under [`CANDIDATES`] whose path
//!    or basename appears in the text of a covered SCRIPT (20 of ~24
//!    MUTATING scripts source or exec a helper — `commission-a-disk.
//!    judge.sh`, `node-roles.sh`, `infra/lib/jq.sh`, …). Comments are
//!    read too: a name is a name. `examples/` is a candidate beside
//!    `infra/` because a MUTATING verb reads it as DATA:
//!    retire-example-reference-rows builds its delete set from
//!    `examples/*/seeds/*` (through `infra/postgres/
//!    example-reference-rows.sh`, which names `classes.json`,
//!    `locations.toml`, …), so a seed edit changes what that verb
//!    deletes (review of 85b04305).
//! 4. the protocols in [`ALWAYS`]: the runner names its packet kind
//!    (`ops-request`) without the file's extension, so no closure finds
//!    the row that decides who may file, approve and claim a request.
//!    And the directories in [`ALWAYS_DIRS`] — the harness roster, whose
//!    files say what `boss dispatch --launch` executes and as whom, and
//!    which nothing names by path (review f7f0b689, B1).
//! 5. every file under [`CANDIDATES`] whose own text says [`CLAIM`] —
//!    that every change to it is a credentials-area car waiting for an
//!    adversarial review — and the CLI module its text names as the
//!    command that reads it (backlog 275d75af). Today that is the
//!    printed re-entry sheet, `infra/recovery/re-entry.toml`, and its
//!    renderer `boss recovery sheet` (`recovery.rs`, `recovery/`): the
//!    sheet's header made the claim (review of car 67e78997, F6) and no
//!    list here named it, so a car changing only the sheet boarded
//!    unheld. The dev pod's login selector, `infra/dev/dev-session.sh`,
//!    makes it too (review a27c860d F4): every interactive ssh login
//!    runs it as root, and it names no renderer and prints from no
//!    file, so it holds itself alone. So does the node-maintenance
//!    manifest, `infra/cluster/manifests/boss-node-maintenance.yaml`
//!    (review 9b05cc55 N8): its pod runs as root with CAP_SYS_ADMIN on
//!    the build node, and it was held only because three lints happened
//!    to name its file. The rule is unchanged; the manifest's header
//!    now makes the sentence.
//!    The sentence IS the list (CLAUDE.md 9a): a file that
//!    makes it is held, a car that deletes it is held by the base that
//!    still makes it, and a test on the real tree fails if the sheet
//!    stops making it. The 17 files the sheet reads FACTS from are not
//!    held: 123 of 642 commits in 30 days touched one (measured
//!    2026-10-01 on origin/main ee9a78a5) against 7 for the sheet and
//!    its renderer, and a resolved value is judged by the renderer's
//!    own key-material refusal and moves the sheet's version, which
//!    asks for a reprint.
//!
//!    The files a claimant prints PROSE from are held, though (backlog
//!    27272837, review 818ac256 F1): the sheet printed the dev-door
//!    JSON's free-text `what`, `command` and `why` (outside `infra/`,
//!    claiming nothing), so a car changing only that file changed
//!    printed sentences unreviewed — and the header says which key
//!    opens which door is enforced by review alone. Derived from the
//!    claimant's own lines ([`prose_sources`]): the `file` of every
//!    line that names `fields`. A single `key`/`pattern` read is a
//!    value, and a real-tree test resolves every one and fails on a
//!    sentence, so the shape cannot quietly stop meaning what it says.
//!
//! The MUTATING word (and `requires_approval`) still names, in the
//! reason, which touched paths belong to a verb that says it acts —
//! the reviewer's first read — but it no longer decides the hold.
//!
//! BOTH SIDES, because a car that demotes a verb's `about`, repoints its
//! `argv` or deletes the verb changes what that verb did, and the tip
//! alone would no longer name the old script. The diff is read with
//! `--no-renames` (a moved script's OLD path counts) and `-z` (a path
//! git would quote is matched as itself).
//!
//! THE BOUND, measured on 2026-09-28 (origin/main 26e3e322): the
//! closure covers 369 of the 898 files under `infra/` and `examples/`
//! (342 of `infra/` alone; 33 of 595 commits in 30 days touched
//! `examples/`). Candidates are held to those two trees, because the
//! whole-tree closure reaches `infra/gate.sh`, which names every
//! crate's `lib.rs` and `Cargo.toml`, and then holds 207 files outside
//! `infra/` — every crate car would wait for a review, and a check
//! that holds everything is read as holding nothing. Only script text
//! is followed (`.sh`, `.bash`, `.py`, or an extensionless file with a
//! shebang); a verb's `about` and a runbook's prose name files as
//! references, and following them doubled the set.
//!
//! THE BOSS CLI A VERB RUNS (backlog b089661d). Verbs reach the `boss`
//! crate with no path in `argv` and none in a script's text:
//! `reach` and `run-car-probe` put `boss` at `argv[0]`, publish-workflow
//! runs `"$BOSS_BIN" workflow publish`, and merge-tenant-main and the
//! converge guard on `"$CLI" tenant check` / `"$cli" tenant check`. So
//! a verb's `argv` and every covered script are also read for the
//! binary followed by a subcommand — `boss`, a path ending `/boss`, or
//! a shell expansion the script ties to it (`$BOSS_BIN`,
//! `${BOSS_CLI:-boss}`, or a `$CLI` whose assignment line says boss),
//! because two of the four spellings on main say `boss` only where the
//! variable is set — and a subcommand that is a module of the crate
//! (`car-retire` is `car_retire`) covers [`CLI_SRC`]`<sub>.rs`,
//! `<sub>/`, and the in-crate modules those name ONE level down
//! (`crate::m`, `super::m`, `m::`). The module names are the listing's,
//! never a list typed here. Comment lines are not read for this: a
//! comment runs nothing, and the prose in them took the set from 31 of
//! 88 CLI files to 59; reading ANY expansion as the binary took it to
//! 47, every addition a false match (`"$DOCKER" inspect`, `$CAR_KIND
//! car`). Measured on the tree of 2026-09-28; 181 of 599 commits in 30
//! days touch the 31 (the triage's estimate for the four named
//! subcommands alone was 115; `boss dispatch`, `job`, `queue`, `ops`,
//! `events` and `estate` in covered scripts are the rest).
//!
//! NOT COVERED. The hubs in [`CLI_HUBS`], by any rule — `gate` and
//! `train` are the CLI's shared plumbing, and following them took the
//! share of 30 days' commits held from ~19% to 37%+ (the triage of
//! b089661d, measured on origin/main b34427e0). The crate root
//! (`main.rs`) wholesale: it holds the clap arms and flag switches that
//! route to every module, so it is held only when the diff's hunks
//! (three lines of context) name a reached module or its
//! `Commands::<Sub>` arm or its `mod <sub>` line, or carry any
//! `#[path` ([`CLI_MAIN`]). Three main.rs shapes judge Clear here:
//! an edit routing a reached subcommand to a new module more than three
//! lines from any line that names it; an early return added to
//! `async fn main` ahead of the dispatch, far from every arm; and a
//! change to a reached subcommand's FLAGS, which live in main.rs's clap
//! definitions (the `Prove` variant, `WorkflowAction`) far from any
//! line naming the module — `dry_run` given `ArgAction::SetFalse`, or
//! `unattended` losing its `requires` / `conflicts_with`. Moving each
//! covered subcommand's clap Args and dispatch into its own module (as
//! `tenant::dispatch` is) is the structural fix, a follow-up of the
//! re-review of 9fe4f350 (R2, R3). A module
//! two levels down, and every crate OUTSIDE boss-cli that a covered
//! module calls (`tenant.rs` reads boss_classes, boss_jobs,
//! boss_policy, …; `workflow.rs` boss_jobs): a car changing those
//! judges Clear here. A binary the script is handed under a name it
//! never ties to boss (`B=$1; "$B" tenant check`). A CLI path in the
//! reason's MUTATING list: the closure is not traced per verb, so it
//! names only the verb files and their scripts.
//!
//! NO EVIDENCE IS NOT A PASS. A diff, a tree or a registry that cannot
//! be read answers [`Judgement::Unread`], which holds too: a hold nobody
//! needed costs one `boss release`, a missed one costs the review.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The ops runner's directory: every file here is covered.
pub(crate) const OPS_DIR: &str = "infra/ops/";

/// Where the verb registry lives, relative to the repo root — the
/// directory the ops runner reads its allowlist from.
pub(crate) const VERBS_DIR: &str = "infra/ops/verbs/";

/// Where the closure looks for the helpers and data a covered script
/// names: `infra/`, and `examples/`, whose seeds a MUTATING verb reads
/// as its delete set.
pub(crate) const CANDIDATES: [&str; 2] = ["infra/", "examples/"];

/// The boss CLI's module sources: listed beside [`CANDIDATES`], and
/// covered only through a subcommand a verb or a covered script runs —
/// never by the basename closure.
pub(crate) const CLI_SRC: &str = "crates/orchestrators/boss-cli/src/";

/// The CLI's shared plumbing, covered by no rule: nearly every module
/// names one of them, so following them holds nearly every CLI car —
/// 37%+ of 30 days' commits against ~19% without (b089661d).
pub(crate) const CLI_HUBS: [&str; 2] = ["gate", "train"];

/// Covered whatever names them: the packet protocol the runner answers,
/// which it names by kind (`ops-request`), never by file.
pub(crate) const ALWAYS: [&str; 1] = ["infra/platform/workflows/ops-request.toml"];

/// Covered whatever names them, by DIRECTORY: every file under one, on
/// either side of the change, so a file added or deleted there holds too.
///
/// `infra/platform/harnesses/` (review f7f0b689, B1). A harness file says
/// what `boss dispatch --launch` executes — its argv, the actor it signs
/// as, its sandbox per profile, the environment it inherits — and nothing
/// names it by path: the launcher reads the directory. A car changing
/// only such a file judged Clear and would have boarded on its green.
pub(crate) const ALWAYS_DIRS: [&str; 1] = ["infra/platform/harnesses/"];

/// The item that asked for this, named in every reason the doors print.
const ITEM: &str = "backlog fdbb447e";

/// Does this verb spec SAY it changes a host? The word MUTATING in its
/// `about` (every such verb opens with it; the lints derive the roster
/// the same way) or `requires_approval: true`. It names paths in the
/// reason; it does not decide the hold.
pub(crate) fn is_mutating(spec: &Value) -> bool {
    spec.get("about")
        .and_then(Value::as_str)
        .is_some_and(|a| a.contains("MUTATING"))
        || spec.get("requires_approval").and_then(Value::as_bool) == Some(true)
}

/// One side of a change: every tracked file under [`CANDIDATES`] and
/// [`CLI_SRC`], by path, with its text (lossy — a binary is a name, not
/// a script).
pub(crate) type Side = BTreeMap<String, String>;

/// One verb file as one side holds it. `spec` is `None` when it does
/// not parse — a verb nobody can read is taken to be MUTATING.
struct Verb<'a> {
    path: &'a str,
    spec: Option<Value>,
}

impl Verb<'_> {
    fn mutating(&self) -> bool {
        self.spec.as_ref().is_none_or(is_mutating)
    }

    /// Every `argv` entry that is a path — `argv[0]` or behind an
    /// interpreter, a `{n}` placeholder never.
    fn runs(&self) -> Vec<String> {
        self.spec
            .as_ref()
            .and_then(|s| s.get("argv"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|a| a.contains('/'))
            .map(str::to_string)
            .collect()
    }
}

fn verbs(side: &Side) -> Vec<Verb<'_>> {
    side.iter()
        .filter(|(p, _)| p.starts_with(VERBS_DIR) && p.ends_with(".json"))
        .map(|(p, text)| Verb {
            path: p,
            spec: serde_json::from_str(text).ok(),
        })
        .collect()
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A file whose text the closure follows.
fn is_script(path: &str, text: &str) -> bool {
    let name = basename(path);
    name.ends_with(".sh")
        || name.ends_with(".bash")
        || name.ends_with(".py")
        || (!name.contains('.') && text.starts_with("#!"))
}

/// PURE: every path one side covers — [`OPS_DIR`], [`ALWAYS`], every
/// verb's path arguments, the closure over the scripts among them, and
/// the boss CLI modules a verb or a covered script runs.
pub(crate) fn covered(side: &Side) -> BTreeSet<String> {
    let mut covered = closure(side);
    let mut reached = reached(side, &covered);
    let Claimed {
        claimants,
        renderers,
        prose,
    } = claimed(side);
    covered.extend(claimants);
    covered.extend(prose);
    reached.extend(renderers);
    covered.extend(
        side.keys()
            .filter(|p| module_of(p).is_some_and(|m| reached.contains(m)))
            .cloned(),
    );
    covered
}

/// The sentence a file's own header writes to say every change to it
/// waits for an adversarial review — `infra/recovery/re-entry.toml`'s,
/// word for word, compared with comment leaders stripped, lines joined
/// and case folded, so a rewrap of the header does not unhold it.
pub(crate) const CLAIM: &str =
    "every change to this file is a credentials-area car and waits for an adversarial review";

/// The text of a file as one line of words: comment leaders (`#`,
/// `//`, `//!`, `///`) stripped, whitespace collapsed, lowercased.
fn prose_of(text: &str) -> String {
    text.lines()
        .map(|l| l.trim_start().trim_start_matches(['#', '/', '!']))
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// PURE: the files under [`CANDIDATES`] whose text makes the [`CLAIM`],
/// and the CLI module each one's text names as the command that reads
/// it (the sheet's header: "`boss recovery sheet` reads this file") —
/// the renderer, which decides what the file's lines print and holds
/// the secret-shape tripwires, never a hub. ONE DEFINITION (CLAUDE.md
/// 9a, backlog 275d75af): the header's claim was the only list, and the
/// hold did not read it, so a car changing the printed sheet alone
/// boarded unheld.
fn claimed(side: &Side) -> Claimed {
    let modules = cli_modules(side);
    let claimants: Vec<(&String, &String)> = side
        .iter()
        // Under [`CANDIDATES`] only: this module spells the sentence as
        // a constant, and a Rust source quoting it is not a claimant.
        .filter(|(p, t)| {
            CANDIDATES.iter().any(|d| p.starts_with(d))
                && t.contains("review")
                && prose_of(t).contains(CLAIM)
        })
        .collect();
    let renderers = claimants
        .iter()
        .flat_map(|(_, t)| t.split("`boss ").skip(1))
        // Only the command the header says READS the file — a `boss
        // prove` named in a road's prose renders nothing.
        .filter(|rest| {
            rest.split_once('`')
                .is_some_and(|(_, after)| after.starts_with(" reads this file"))
        })
        .filter_map(|rest| rest.split([' ', '`']).next())
        .map(|sub| sub.replace('-', "_"))
        .filter(|sub| modules.contains(sub.as_str()) && !CLI_HUBS.contains(&sub.as_str()))
        .collect();
    let prose = claimants
        .iter()
        .flat_map(|(_, t)| prose_sources(t))
        .collect();
    Claimed {
        claimants: claimants.into_iter().map(|(p, _)| p.clone()).collect(),
        renderers,
        prose,
    }
}

/// What one side's [`CLAIM`] lends its hold to.
struct Claimed {
    /// The files whose own text makes the claim.
    claimants: BTreeSet<String>,
    /// The CLI module each names as the command that reads it.
    renderers: BTreeSet<String>,
    /// The files a claimant prints PROSE from ([`prose_sources`]).
    prose: BTreeSet<String>,
}

/// PURE: the files a claimant sheet prints PROSE from — the `file` of
/// every `[[road.line]]` that names `fields` (backlog 27272837, review
/// 818ac256 F1). Read off the claimant's own lines, never a list typed
/// here (CLAUDE.md 9a): today the dev-door JSON, whose `what`, `command`
/// and `why` print as sentences on the re-entry sheet, so a car editing
/// only that file changed printed prose with no review.
///
/// PROSE FROM A VALUE, by the line's shape: `fields` prints a list of
/// records field by field — free text. Every other read (a line's
/// `key` or `pattern`, a command's `with`, a road's `needs`) reads ONE
/// value — an address, a port, a tag — and stays unheld as backlog
/// 275d75af decided. The shape is only as good as what it resolves
/// to, so a real-tree test resolves every such read with the renderer
/// and fails on any that reads as a sentence (three or more words).
/// A claimant that does not parse lends nothing here; it is itself
/// held, and the other side of the change still names its sources.
fn prose_sources(text: &str) -> BTreeSet<String> {
    let Ok(doc) = toml::from_str::<toml::Value>(text) else {
        return BTreeSet::new();
    };
    doc.get("road")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|road| road.get("line").and_then(toml::Value::as_array))
        .flatten()
        .filter(|line| line.get("fields").is_some())
        .filter_map(|line| line.get("file").and_then(toml::Value::as_str))
        .map(str::to_string)
        .collect()
}

/// PURE: [`OPS_DIR`], [`ALWAYS`], every verb's path arguments, and the
/// basename closure over the scripts among them — `infra/` and
/// `examples/` only.
fn closure(side: &Side) -> BTreeSet<String> {
    let mut covered: BTreeSet<String> = side
        .keys()
        .filter(|p| p.starts_with(OPS_DIR) || ALWAYS_DIRS.iter().any(|d| p.starts_with(d)))
        .cloned()
        .collect();
    covered.extend(ALWAYS.iter().map(|p| p.to_string()));
    covered.extend(verbs(side).iter().flat_map(Verb::runs));
    let mut queue: Vec<String> = covered.iter().cloned().collect();
    while let Some(path) = queue.pop() {
        let Some(text) = side.get(&path).filter(|t| is_script(&path, t)) else {
            continue;
        };
        // The basename is inside the path, so one test answers both.
        let found: Vec<String> = side
            .keys()
            .filter(|c| {
                CANDIDATES.iter().any(|d| c.starts_with(d))
                    && !covered.contains(*c)
                    && text.contains(basename(c))
            })
            .cloned()
            .collect();
        for c in found {
            covered.insert(c.clone());
            queue.push(c);
        }
    }
    covered
}

/// PURE: the CLI modules a verb's argv or a script in `closure` runs,
/// and those they name one level down — never a hub.
fn reached(side: &Side, closure: &BTreeSet<String>) -> BTreeSet<String> {
    let modules = cli_modules(side);
    let argvs = verbs(side).into_iter().filter_map(|v| {
        v.spec
            .as_ref()
            .and_then(|s| s.get("argv"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
    });
    let scripts = closure
        .iter()
        .filter_map(|p| side.get(p).filter(|t| is_script(p, t)).cloned());
    let run: BTreeSet<String> = argvs
        .chain(scripts)
        .flat_map(|text| subcommands(&text, &modules))
        .collect();
    run.iter()
        .flat_map(|sub| {
            let text: String = side
                .iter()
                .filter(|(p, _)| module_of(p) == Some(sub.as_str()))
                .map(|(_, t)| t.as_str())
                .collect();
            let mut named: Vec<&str> = modules
                .iter()
                .copied()
                .filter(|m| names_module(&text, m))
                .collect();
            named.push(sub.as_str());
            named
        })
        .filter(|m| !CLI_HUBS.contains(m))
        .map(str::to_string)
        .collect()
}

/// The crate root: it declares the modules AND routes to them — the
/// clap arms (`Commands::Prove { .. } => …`) and the flag switches
/// (`if unattended { return prove::run_unattended(..) }`). Held only
/// when the diff's hunks name a reached module, because it is in ~112
/// of ~600 commits in 30 days (review of 5cf58144, finding 2). By this
/// rule 65 of those 112 hold, and 10 of the 65 were not held already by
/// a covered module file they also touched (measured 2026-09-29).
pub(crate) const CLI_MAIN: &str = "crates/orchestrators/boss-cli/src/main.rs";

/// Lines of context around each change in [`CLI_MAIN`] read by
/// [`main_routes_to`]: a line added beside an arm (`return evil::run()`
/// just above `prove::run_unattended`) names nothing reached itself.
const MAIN_CONTEXT: &str = "-U3";

/// PURE: do the hunks of a diff of [`CLI_MAIN`] — changed lines and
/// their context — name one of the `reached` modules, as `m::`,
/// `crate::m`, its declaration `mod m`, or its clap arm `Commands::M`
/// (`car_retire` is `CarRetire`)? Any `#[path` holds on its own: it
/// re-points a module's source at a file nothing else names. Hunk
/// headers and file headers are not read.
fn main_routes_to(hunks: &str, reached: &BTreeSet<String>) -> bool {
    let body: String = hunks
        .lines()
        .filter(|l| {
            !(l.starts_with("@@")
                || l.starts_with("+++")
                || l.starts_with("---")
                || l.starts_with("diff ")
                || l.starts_with("index "))
        })
        .collect::<Vec<_>>()
        .join("\n");
    // `#[path = "evil.rs"] mod prove;` swaps a module's whole source
    // under its own name, and the file it names may be any file at all:
    // a `#[path` anywhere in the hunks holds (re-review of 9fe4f350, R1).
    if body.contains("#[path") {
        return true;
    }
    let whole = |needle: &str| {
        body.match_indices(needle).any(|(i, _)| {
            !body[..i].chars().next_back().is_some_and(is_ident)
                && !body[i + needle.len()..].starts_with(is_ident)
        })
    };
    reached.iter().any(|m| {
        let arm: String = m
            .split('_')
            .map(|w| {
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                    .unwrap_or_default()
            })
            .collect();
        names_module(&body, m) || whole(&format!("mod {m}")) || whole(&format!("Commands::{arm}"))
    })
}

/// PURE: `judged`, with [`CLI_MAIN`] added to what it touches when the
/// car changes it and `main_hunks` route to a module a verb reaches on
/// either side.
fn with_main(
    judged: Judgement,
    changed: &[String],
    sides: &[&Side],
    main_hunks: &str,
) -> Judgement {
    if !changed.iter().any(|p| p == CLI_MAIN) {
        return judged;
    }
    let reached: BTreeSet<String> = sides
        .iter()
        .flat_map(|s| {
            let mut r = reached(s, &closure(s));
            r.extend(claimed(s).renderers);
            r
        })
        .collect();
    if !main_routes_to(main_hunks, &reached) {
        return judged;
    }
    match judged {
        Judgement::Clear => Judgement::Touches {
            paths: vec![CLI_MAIN.to_string()],
            mutating: Vec::new(),
        },
        Judgement::Touches {
            mut paths,
            mutating,
        } => {
            paths.push(CLI_MAIN.to_string());
            paths.sort();
            paths.dedup();
            Judgement::Touches { paths, mutating }
        }
        unread => unread,
    }
}

/// The module a path under [`CLI_SRC`] belongs to — `x` for `x.rs` and
/// for anything under `x/`. The crate root is no module: it routes to
/// every one, so it is judged by its hunks ([`CLI_MAIN`]), not wholesale.
fn module_of(path: &str) -> Option<&str> {
    let rest = path.strip_prefix(CLI_SRC)?;
    let name = rest.split('/').next()?;
    let name = name.strip_suffix(".rs").unwrap_or(name);
    (!name.is_empty() && name != "main" && name != "lib").then_some(name)
}

/// Every module name the side's [`CLI_SRC`] listing holds.
fn cli_modules(side: &Side) -> BTreeSet<&str> {
    side.keys().filter_map(|p| module_of(p)).collect()
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The CLI modules `text` runs: a word that is the binary — `boss`, a
/// path ending `/boss`, or an expansion of it ([`names_the_binary`]) —
/// followed by a word that names a module (a hyphen read as `_`).
/// Words split where the shell splits a command (`$(`, `;`, `|`, `&&`,
/// backticks), quotes stripped, so `out=$("$cli" tenant check` is read
/// as `$cli tenant`. A comment line runs nothing and is skipped.
fn subcommands(text: &str, modules: &BTreeSet<&str>) -> Vec<String> {
    let words: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(|l| {
            l.split(|c: char| c.is_whitespace() || matches!(c, ';' | '|' | '&' | '(' | ')' | '`'))
        })
        .map(|w| w.trim_matches(|c| c == '"' || c == '\''))
        .filter(|w| !w.is_empty())
        .collect();
    words
        .windows(2)
        .filter(|w| w[0] == "boss" || w[0].ends_with("/boss") || names_the_binary(text, w[0]))
        .map(|w| w[1].replace('-', "_"))
        .filter(|sub| modules.contains(sub.as_str()))
        .collect()
}

/// Is `word` a shell expansion of the binary? `$X`, `${X…}` whose
/// name or default says boss (`$BOSS_BIN`, `${BOSS_CLI:-boss}`), or
/// whose name the script assigns on a line that says boss
/// (`CLI="${BOSS_CLI:-boss}"`, `local … cli="${BOSS_CLI:-boss}"`).
fn names_the_binary(text: &str, word: &str) -> bool {
    let Some(rest) = word.strip_prefix('$') else {
        return false;
    };
    let rest = rest.strip_prefix('{').unwrap_or(rest);
    let name: String = rest.chars().take_while(|c| is_ident(*c)).collect();
    if name.is_empty() {
        return false;
    }
    if word.to_ascii_lowercase().contains("boss") {
        return true;
    }
    let assign = format!("{name}=");
    text.lines().any(|line| {
        line.contains("boss")
            && line.match_indices(&assign).any(|(i, _)| {
                !line[..i]
                    .chars()
                    .next_back()
                    .is_some_and(|c| is_ident(c) || c == '$' || c == '{')
            })
    })
}

/// Does a module's source name module `m` — `crate::m`, `super::m`, or
/// `m::` where no path or identifier runs into it?
fn names_module(text: &str, m: &str) -> bool {
    let qualified = ["crate::", "super::"].iter().any(|q| {
        let needle = format!("{q}{m}");
        text.match_indices(&needle)
            .any(|(i, _)| !text[i + needle.len()..].starts_with(is_ident))
    });
    let bare = format!("{m}::");
    qualified
        || text.match_indices(&bare).any(|(i, _)| {
            !text[..i]
                .chars()
                .next_back()
                .is_some_and(|c| is_ident(c) || c == ':')
        })
}

/// What a car's diff says about the ops verbs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Judgement {
    /// It touches nothing covered.
    Clear,
    /// It touches `paths`; `mutating` is the subset that is a verb
    /// saying MUTATING, or a path such a verb runs.
    Touches {
        paths: Vec<String>,
        mutating: Vec<String>,
    },
    /// The diff, a tree or the registry could not be read; the cause.
    Unread(String),
}

/// PURE: judge `changed` against both sides. A pair of sides with no
/// verb at all is the registry MOVED, not a car that touches none: a
/// judgement over nothing would pass every car.
pub(crate) fn touched(changed: &[String], sides: &[&Side]) -> Judgement {
    if sides.iter().all(|s| verbs(s).is_empty()) {
        return Judgement::Unread(format!(
            "no side of the change holds a verb under {VERBS_DIR} — the registry moved, and a \
             judgement over nothing would pass every car"
        ));
    }
    let covers: BTreeSet<String> = sides.iter().flat_map(|s| covered(s)).collect();
    let acts: BTreeSet<String> = sides
        .iter()
        .flat_map(|s| {
            verbs(s)
                .into_iter()
                .filter(Verb::mutating)
                .flat_map(|v| {
                    let mut named = v.runs();
                    named.push(v.path.to_string());
                    named
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let paths: Vec<String> = changed
        .iter()
        .filter(|p| covers.contains(*p))
        .cloned()
        .collect();
    if paths.is_empty() {
        return Judgement::Clear;
    }
    Judgement::Touches {
        mutating: paths
            .iter()
            .filter(|p| acts.contains(*p))
            .cloned()
            .collect(),
        paths,
    }
}

impl Judgement {
    /// A FINDING: the diff was read and touches what a review must see.
    /// An unread diff holds too, but it is not a finding — it is judged
    /// again, never sent to a reviewer (backlog b7b02024, review F1).
    pub(crate) fn is_finding(&self) -> bool {
        matches!(self, Judgement::Touches { .. })
    }

    /// THE predicate: the hold this car waits under, or `None` when it
    /// may board on its green alone. Plain text, no quote a shell would
    /// read: it is printed beside the `--hold` a builder might retype.
    pub(crate) fn hold_reason(&self) -> Option<String> {
        match self {
            Judgement::Clear => None,
            Judgement::Touches { paths, mutating } => Some(format!(
                "touches an ops verb, what a verb runs, the runner, a harness file, or a \
                 file that claims a review, its renderer, or a file it prints prose from \
                 ({}){}: waits for its \
                 adversarial review; boss release records the RELEASE verdict ({ITEM})",
                paths.join(", "),
                if mutating.is_empty() {
                    String::new()
                } else {
                    format!(" — MUTATING: {}", mutating.join(", "))
                }
            )),
            Judgement::Unread(why) => Some(format!(
                "whether its diff touches an ops verb could not be read ({why}): held until \
                 someone reads it; boss release once they have ({ITEM})"
            )),
        }
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "git {}: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("no stderr")
        ))
    }
}

/// Every object `rev:<path>` names, in one `git cat-file --batch` —
/// `None` for a path git answers `missing`.
fn cat_batch(repo: &Path, rev: &str, paths: &[&str]) -> Result<Vec<Option<String>>, String> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run git cat-file: {e}"))?;
    let input: String = paths.iter().map(|p| format!("{rev}:{p}\n")).collect();
    let mut stdin = child.stdin.take().ok_or("git cat-file took no stdin")?;
    // Written from a thread: a batch larger than the pipe would block
    // the write while git blocks on the unread output.
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git cat-file: {e}"))?;
    writer
        .join()
        .map_err(|_| "git cat-file: the writer panicked".to_string())?
        .map_err(|e| format!("git cat-file: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git cat-file: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut rest: &[u8] = &out.stdout;
    let mut blobs = Vec::with_capacity(paths.len());
    for path in paths {
        let eol = rest
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| format!("git cat-file: the answer for {path} is cut short"))?;
        let header = String::from_utf8_lossy(&rest[..eol]).into_owned();
        rest = &rest[eol + 1..];
        if header.ends_with(" missing") {
            blobs.push(None);
            continue;
        }
        let size: usize = header
            .rsplit(' ')
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("git cat-file: unreadable header {header:?}"))?;
        let body = rest
            .get(..size)
            .ok_or_else(|| format!("git cat-file: {path} is cut short"))?;
        blobs.push(Some(String::from_utf8_lossy(body).into_owned()));
        rest = rest.get(size + 1..).unwrap_or_default();
    }
    Ok(blobs)
}

/// The [`Side`] `rev` holds.
fn side_at(repo: &Path, rev: &str) -> Result<Side, String> {
    let mut args = vec!["ls-tree", "-r", "-z", "--name-only", rev, "--"];
    args.extend(CANDIDATES);
    args.push(CLI_SRC);
    let listed = git(repo, &args)?;
    let paths: Vec<&str> = listed.split('\0').filter(|p| !p.is_empty()).collect();
    let blobs = cat_batch(repo, rev, &paths)?;
    Ok(paths
        .into_iter()
        .zip(blobs)
        .filter_map(|(p, b)| b.map(|text| (p.to_string(), text)))
        .collect())
}

/// Read the car's diff `base..tip` in `repo` and judge it. `base` is
/// the car's merge-base with main, so the diff is the car's own change
/// and not what main did since. An empty `base` is an unread diff.
pub(crate) fn judge(repo: &Path, base: &str, tip: &str) -> Judgement {
    if base.trim().is_empty() {
        return Judgement::Unread("the car's base with main could not be resolved".into());
    }
    let read = || -> Result<Judgement, String> {
        let diff = git(
            repo,
            &["diff", "--no-renames", "-z", "--name-only", base, tip, "--"],
        )?;
        let changed: Vec<String> = diff
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
        if changed.is_empty() {
            return Ok(Judgement::Clear);
        }
        let (old, new) = (side_at(repo, base)?, side_at(repo, tip)?);
        let judged = touched(&changed, &[&old, &new]);
        let main_hunks = if changed.iter().any(|p| p == CLI_MAIN) {
            git(
                repo,
                &[
                    "diff",
                    "--no-renames",
                    MAIN_CONTEXT,
                    base,
                    tip,
                    "--",
                    CLI_MAIN,
                ],
            )?
        } else {
            String::new()
        };
        Ok(with_main(judged, &changed, &[&old, &new], &main_hunks))
    };
    read().unwrap_or_else(Judgement::Unread)
}

/// `boss gate`'s half: the hold the gate stamps. A park-intent gate
/// whose diff the predicate holds carries the predicate's reason —
/// APPENDED to a `--hold` it was given, so the reviewer reads both why
/// a person held it and what the diff touches. A gate with no park
/// intent files no car, so it stamps none of its own — a hold there
/// would read its forgotten green as deliberately waiting.
pub(crate) fn gate_hold(
    given: Option<String>,
    park_intent: bool,
    judged: &Judgement,
) -> Option<String> {
    match (given, judged.hold_reason().filter(|_| park_intent)) {
        (Some(given), Some(reason)) => Some(format!("{given}; {reason}")),
        (given, reason) => given.or(reason),
    }
}

/// The dock's half, PURE: the write that holds a docked `car` under
/// `reason` — `(path, body)` for the review step's merge door, the
/// write `boss hold` makes (`steps::hold_patch`) — or `None` when the
/// car is held already, whose reason then stands. A car with no open
/// review step cannot carry the hold, and that is a refusal: a door
/// that cannot hold the car must not make it boardable.
pub(crate) fn car_hold_write(car: &Value, reason: &str) -> Result<Option<(String, Value)>, String> {
    let review = crate::steps::holdable(car)?;
    if boss_jobs::stranded::hold_reason(review.get("metadata").unwrap_or(&Value::Null)).is_some() {
        return Ok(None);
    }
    let car_id = car.get("id").and_then(Value::as_str).unwrap_or_default();
    let step_id = review.get("id").and_then(Value::as_str).unwrap_or_default();
    if car_id.is_empty() || step_id.is_empty() {
        return Err("the car or its review step carries no id to write the hold to".into());
    }
    Ok(Some((
        format!("/api/jobs/{car_id}/steps/{step_id}/metadata"),
        crate::steps::hold_patch(reason),
    )))
}

/// Where a review hold records the car head it was judged at, beside
/// `hold` on the review step (design 7cedfa29 D2) — core's one spelling.
pub(crate) const HOLD_SHA: &str = boss_jobs::car::HOLD_SHA;

/// THE CONDUCTOR'S HALF — the one road every car travels (backlog
/// b7b02024 finding 8, design 7cedfa29 D1). The CLI doors above judge a
/// car only when a car comes through them; an older installed binary, a
/// hand-PATCHed park intent, the conductor's own dock re-gate and the
/// dispatcher's rerail-a-car rule all reach the dock without them, and
/// `parked_ready` read only the marker, so such a car boarded unheld.
/// The conductor re-judges every parked car's diff where it boards.
///
/// PURE, with the judge handed in lazily. A RELEASE IS A RECORD BOUND TO
/// A HEAD (design 7cedfa29 D6, backlog b7b02024 finding 7):
///
/// - a release recorded at THIS head is checked against the review it
///   names ([`Dock::Check`] — one GET, `review_verdict::vouches`), and
///   nothing is judged: the review read this diff;
/// - a release recorded at another head is no release of this one —
///   the car is held for a re-review;
/// - a car that carries a [`HOLD_SHA`] and no release was held for its
///   review and cleared without one (a bare `boss release`, a raw
///   metadata PATCH) — held again;
/// - any other car is judged, and a clear diff boards.
///
/// None of this asks WHO wrote a key: every agent signs as
/// `agent-claude` and the shared token lets anyone type any actor, so
/// the check stands on a sha and an artifact instead.
///
/// A RELEASE SURVIVES THE DOCK'S OWN REPLAY, AND NOTHING ELSE (review
/// F3). The dock re-gate replays a released car onto main and so gives
/// it a new head; re-holding it there would ask for a second review of
/// a mechanical rebase every time main moved. The release is carried
/// only when the car's re-gate stamp records the replay FROM the
/// release's head TO this one and `same_change(from, to)` proves the
/// car's own diff identical (patch-ids). Anything else is a new head.
///
/// ONLY A FINDING BINDS A HOLD TO A HEAD (review F1). A hold the judge
/// placed because it could not READ the diff (a git blip) is written
/// without [`HOLD_SHA`] (`Dock::Hold { bind: false }`), so clearing it
/// sends the car back to the judge rather than to a reviewer.
pub(crate) fn dock(
    car: &Value,
    head: &str,
    judge: impl FnOnce() -> Judgement,
    same_change: impl FnOnce(&str, &str) -> Result<String, String>,
) -> Dock {
    let at = |why: String| Dock::Hold {
        reason: format!("conductor re-judge at {}: {why}", short(head)),
        bind: true,
    };
    let review_md =
        boss_jobs::car::find_step(car, boss_jobs::car::REVIEW_SLUG, boss_jobs::car::REVIEW)
            .and_then(|s| s.get("metadata"))
            .unwrap_or(&Value::Null);
    if let Some(release) = crate::review_verdict::release_on(car) {
        let check = |carry: Option<String>| Dock::Check {
            review: release.review.to_string(),
            reviewed: release.reviewed.to_string(),
            carry,
        };
        if head.is_empty() || release.review.is_empty() {
            return at(format!(
                "its release names no review, or its head could not be read ({ITEM_B7})"
            ));
        }
        // THE RECORD IS A LABEL; THE PATCH-ID IS THE PROOF (re-review N1).
        // `reviewed_sha` is plain step metadata: one PATCH could write a
        // release at a new head naming a review of an old one. So a
        // release whose review READ another head than this one boards
        // only while the car's own diff at this head has the patch-id of
        // the diff that review read — asked on every pass, never taken
        // on the record's word.
        if release.sha == head {
            if release.reviewed == head {
                return check(None);
            }
            return match same_change(release.reviewed, head) {
                Ok(_) => check(None),
                Err(why) => at(format!(
                    "its release names a review of {}, not of this head, and {why}: waits for \
                     a review of this head; boss review, then boss release --review ({ITEM_B7})",
                    short(release.reviewed)
                )),
            };
        }
        let replayed = crate::train::dock_regate::RegateStamp::of(car)
            .is_some_and(|s| s.head == head && s.from == release.sha);
        let why_not = if replayed {
            match same_change(release.reviewed, head) {
                Ok(patch_id) => return check(Some(patch_id)),
                Err(why) => format!("the dock replayed it, but {why}"),
            }
        } else {
            "no dock replay records this head as a replay of that one".to_string()
        };
        return at(format!(
            "released at {}, head is {} ({why_not}): waits for a review of this head; boss \
             review, then boss release --review ({ITEM_B7})",
            short(release.sha),
            short(head)
        ));
    }
    if let Some(held_at) = review_md
        .get(HOLD_SHA)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return at(format!(
            "held for its review at {} and cleared with no release naming a review: boss \
             review, then boss release --review ({ITEM_B7})",
            short(held_at)
        ));
    }
    let judged = judge();
    match judged.hold_reason() {
        Some(r) => Dock::Hold {
            reason: format!("conductor re-judge at {}: {r}", short(head)),
            bind: judged.is_finding(),
        },
        None => Dock::Board,
    }
}

/// What the dock does with a parked car.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Dock {
    /// It boards on its green.
    Board,
    /// Its release names `review`, which read `reviewed`: it boards when
    /// that run's recorded verdict vouches for the car at `reviewed`.
    /// `carry` is the patch-id that carried the release over the dock's
    /// replay to this head, when it was carried — the conductor then
    /// records the release at this head.
    Check {
        review: String,
        reviewed: String,
        carry: Option<String>,
    },
    /// It is held, for this reason. `bind`: the hold records the head it
    /// was judged at ([`HOLD_SHA`]) — true for a finding and for a
    /// release that does not stand, false for a diff that could not be
    /// read, which is judged again once the hold is cleared.
    Hold { reason: String, bind: bool },
}

/// The car's own change at `sha`, as a patch-id: the diff from its
/// merge-base with `main` to `sha`, through `git patch-id --verbatim`
/// (line numbers ignored; whitespace and context KEPT). Two heads with
/// one patch-id carry the same change onto different bases.
///
/// VERBATIM, NOT STABLE (re-review N2, measured 2026-09-28): `--stable`
/// drops whitespace, and a YAML re-indent moving a key out of its parent
/// gave the SAME id on both heads (ce26d30e46cc). In YAML, Python or a
/// Makefile whitespace is meaning, and this id is the whole proof a
/// carried release stands on. `--verbatim` needs git 2.39 or later.
pub(crate) fn patch_id(repo: &Path, main: &str, sha: &str) -> Result<String, String> {
    use std::io::Write;
    use std::process::Stdio;
    let base = git(repo, &["merge-base", main, sha])?;
    let diff = git(repo, &["diff", "--no-renames", base.trim(), sha])?;
    if diff.trim().is_empty() {
        return Err(format!("{} carries no change of its own", short(sha)));
    }
    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["patch-id", "--verbatim"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run git patch-id: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("git patch-id took no stdin")?;
    let writer = std::thread::spawn(move || stdin.write_all(diff.as_bytes()));
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git patch-id: {e}"))?;
    writer
        .join()
        .map_err(|_| "git patch-id: the writer panicked".to_string())?
        .map_err(|e| format!("git patch-id: {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
        .filter(|_| out.status.success())
        .ok_or_else(|| format!("git patch-id gave no id for {}", short(sha)))
}

/// Is the car's own change the same at `from` and `to`? `Ok(patch-id)`
/// when both heads carry one patch-id; the reason otherwise.
pub(crate) fn same_change(repo: &Path, main: &str, from: &str, to: &str) -> Result<String, String> {
    let (a, b) = (patch_id(repo, main, from)?, patch_id(repo, main, to)?);
    if a == b {
        Ok(a)
    } else {
        Err(format!(
            "its own diff changed in the replay (patch-id {} became {})",
            short(&a),
            short(&b)
        ))
    }
}

/// The item the dock's release rule answers.
const ITEM_B7: &str = "backlog b7b02024";

fn short(sha: &str) -> &str {
    &sha[..12.min(sha.len())]
}

/// The dock's write, PURE: [`car_hold_write`] with the head it was
/// judged at recorded beside the marker, in the same one-body merge, so
/// the hold and the head it names cannot land apart. `head` is `None`
/// for a hold that is not a review's — a diff that could not be read —
/// which must be judged again once cleared, not reviewed (review F1).
pub(crate) fn review_hold_write(
    car: &Value,
    reason: &str,
    head: Option<&str>,
) -> Result<Option<(String, Value)>, String> {
    Ok(car_hold_write(car, reason)?.map(|(path, mut body)| {
        if let Some(head) = head {
            body[HOLD_SHA] = Value::String(head.to_string());
        }
        (path, body)
    }))
}

/// [`judge`] a car at `tip` against `main` in `repo`: the base is their
/// merge-base, so the diff is the car's own change and not what main
/// did since. A tip or a base git cannot resolve is an unread diff —
/// held, never clear.
pub(crate) fn judge_on_main(repo: &Path, main: &str, tip: &str) -> Judgement {
    if tip.trim().is_empty() {
        return Judgement::Unread("the car's head could not be read".into());
    }
    match git(repo, &["merge-base", main, tip]) {
        Ok(base) => judge(repo, base.trim(), tip),
        Err(e) => Judgement::Unread(format!("the car's base with {main}: {e}")),
    }
}

/// `boss park`'s refusal: a car the predicate holds is not parked by
/// the verb that cannot hold it. `None` when the judgement is clear.
pub(crate) fn park_refusal(branch: &str, judged: &Judgement) -> Option<String> {
    let reason = judged.hold_reason()?;
    Some(format!(
        "boss park: REFUSED — {branch} {reason}. This verb files and refreshes cars without \
         a hold, so a car parked here would board before that review. Park it through the \
         doors that hold it: `boss gate {branch} --wait --park-*` (the car is filed HELD), or, \
         for a car already at the dock, a re-gate and then `boss rerail <car> --finish`"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn car(review: Value) -> Value {
        json!({ "id": "c0ffee00-0000-0000-0000-000000000000", "kind": "ship-a-change",
                "status": "open", "steps": [review] })
    }

    fn review(status: &str, md: Value) -> Value {
        json!({ "id": "5e5e5e5e", "title": boss_jobs::car::REVIEW,
                "spec_slug": boss_jobs::car::REVIEW_SLUG, "status": status, "metadata": md })
    }

    fn paths(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    fn side(files: &[(&str, &str)]) -> Side {
        files
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect()
    }

    fn verb(about: &str, argv: &[&str]) -> String {
        json!({ "about": about, "hosts": ["forge"], "argv": argv }).to_string()
    }

    fn touches(p: &[&str], m: &[&str]) -> Judgement {
        Judgement::Touches {
            paths: paths(p),
            mutating: paths(m),
        }
    }

    /// A HARNESS FILE IS HELD (review f7f0b689, B1). A file under
    /// infra/platform/harnesses decides what `boss dispatch --launch`
    /// executes, as which actor, with which sandbox and environment, and
    /// a car changing ONLY such a file — or adding or deleting one — used
    /// to judge Clear and board on its green.
    #[test]
    fn a_car_that_changes_only_a_harness_file_is_held() {
        let df = verb("read: disk", &["df"]);
        let without = side(&[("infra/ops/verbs/df.json", &df)]);
        let with = side(&[
            ("infra/ops/verbs/df.json", &df),
            (
                "infra/platform/harnesses/codex-exec.toml",
                "id = \"codex-exec\"\n",
            ),
            ("infra/platform/stations/repair.toml", "name = \"repair\"\n"),
        ]);
        let file = "infra/platform/harnesses/codex-exec.toml";
        for (what, before, after) in [
            ("changed", &with, &with),
            ("added", &without, &with),
            ("deleted", &with, &without),
        ] {
            let judged = touched(&paths(&[file]), &[before, after]);
            assert_eq!(judged, touches(&[file], &[]), "{what}");
            let hold = judged.hold_reason().expect("held");
            assert!(
                hold.contains(file) && hold.contains("harness"),
                "{what}: {hold}"
            );
        }
        // The neighbouring registry data is not swept in with it.
        assert_eq!(
            touched(
                &paths(&["infra/platform/stations/repair.toml"]),
                &[&with, &with]
            ),
            Judgement::Clear
        );
    }

    #[test]
    fn the_dock_holds_an_unheld_car_on_its_review_step_and_leaves_a_held_one_alone() {
        let (path, body) = car_hold_write(&car(review("ready", json!({}))), "touches x")
            .expect("an open review takes the hold")
            .expect("an unheld car owes the write");
        assert_eq!(
            path,
            "/api/jobs/c0ffee00-0000-0000-0000-000000000000/steps/5e5e5e5e/metadata"
        );
        assert_eq!(
            boss_jobs::stranded::hold_reason(&body).as_deref(),
            Some("touches x"),
            "the shape every dock reader reads"
        );
        // A released hold (`false`) is no hold: the car is held again.
        assert!(
            car_hold_write(&car(review("ready", json!({"hold": false}))), "r")
                .unwrap()
                .is_some()
        );
        // Held already — by an operator or the gate — the reason stands.
        assert_eq!(
            car_hold_write(&car(review("ready", json!({"hold": "area policy"}))), "r"),
            Ok(None)
        );
        // Past review: nothing can hold it, and that is said.
        assert!(car_hold_write(&car(review("completed", json!({}))), "r").is_err());
    }

    const H1: &str = "1111111111111111111111111111111111111111";
    const H2: &str = "2222222222222222222222222222222222222222";

    const REVIEWER: &str = "5eed5eed-0000-4000-8000-000000000000";

    /// A review step recording a release at `sha` that names REVIEWER.
    fn released(sha: &str) -> Value {
        json!({ boss_jobs::car::RELEASE: {"sha": sha, "review": REVIEWER, "verdict": "release",
                "by": "emp-david", "at": "t", "main_sha": H2} })
    }

    /// A same-change check nobody should reach.
    fn unasked(_: &str, _: &str) -> Result<String, String> {
        panic!("no replay is asked about here")
    }

    fn hold(d: Dock) -> (String, bool) {
        match d {
            Dock::Hold { reason, bind } => (reason, bind),
            other => panic!("held, not {other:?}"),
        }
    }

    /// THE DOCK RE-JUDGES EVERY CAR, AND A RELEASE IS A VERDICT AT A HEAD
    /// (backlog b7b02024 cars 1 and 2). A car that reached the dock unheld
    /// is judged; a release at the head the fork carries is checked
    /// against the review it names and nothing is judged; a release at
    /// another head, or a review hold cleared with no release, is held.
    #[test]
    fn the_dock_judges_a_car_and_boards_a_release_only_at_its_head() {
        let ops = || touches(&["infra/forge/x.sh"], &[]);
        let never = || -> Judgement { panic!("a car with a release is not judged") };

        let (unheld, bind) = hold(dock(&car(review("ready", json!({}))), H1, ops, unasked));
        assert!(
            unheld.starts_with("conductor re-judge at 111111111111: "),
            "{unheld}"
        );
        assert!(unheld.contains("infra/forge/x.sh"), "{unheld}");
        assert!(bind, "a finding binds the hold to the head it read");

        assert_eq!(
            dock(&car(review("ready", released(H1))), H1, never, unasked),
            Dock::Check {
                review: REVIEWER.into(),
                reviewed: H1.into(),
                carry: None,
            },
            "released at H1: the review it names is asked, the diff is not"
        );
        let (moved, bind) = hold(dock(
            &car(review("ready", released(H1))),
            H2,
            never,
            unasked,
        ));
        assert!(
            moved.contains("released at 111111111111, head is 222222222222"),
            "{moved}"
        );
        assert!(moved.contains("no dock replay"), "{moved}");
        assert!(moved.contains("boss review"), "{moved}");
        assert!(bind);

        for cleared in [json!({HOLD_SHA: H1}), json!({HOLD_SHA: H1, "hold": null})] {
            let (bare, bind) = hold(dock(
                &car(review("ready", cleared.clone())),
                H1,
                never,
                unasked,
            ));
            assert!(bare.contains("cleared with no release"), "{bare}");
            assert!(bind);
        }

        // A clear diff boards.
        assert_eq!(
            dock(
                &car(review("ready", json!({}))),
                H1,
                || Judgement::Clear,
                unasked
            ),
            Dock::Board
        );
        // An empty head is never the head a release was given at.
        assert!(matches!(
            dock(&car(review("ready", released(""))), "", never, unasked),
            Dock::Hold { .. }
        ));
    }

    /// AN UNREAD DIFF HOLDS, BUT IT IS NOT A FINDING (review F1). Its hold
    /// is written with no `hold_sha`, so a bare release sends the car back
    /// to the judge — which reads it this time and boards a docs car —
    /// rather than to a reviewer for a git blip.
    #[test]
    fn an_unread_diff_holds_unbound_and_is_judged_again_once_cleared() {
        let (unread, bind) = hold(dock(
            &car(review("ready", json!({}))),
            H1,
            || Judgement::Unread("git diff: bad revision".into()),
            unasked,
        ));
        assert!(unread.contains("git diff: bad revision"), "{unread}");
        assert!(!bind, "a blip is not bound to a head");
        let (_, body) = review_hold_write(&car(review("ready", json!({}))), &unread, None)
            .unwrap()
            .unwrap();
        assert!(body.get(HOLD_SHA).is_none(), "{body}");
        // Cleared bare (the review step keeps no hold_sha): judged again.
        let mut md = body.clone();
        md["hold"] = Value::Null;
        assert_eq!(
            dock(&car(review("ready", md)), H1, || Judgement::Clear, unasked),
            Dock::Board
        );
    }

    /// A RELEASE SURVIVES THE DOCK'S OWN REPLAY, AND NOTHING ELSE (review
    /// F3). Carried only when the stamp records the replay FROM the
    /// release's head TO this one AND the car's own diff is unchanged.
    #[test]
    fn a_release_is_carried_over_the_docks_replay_only_when_the_change_is_the_same() {
        let replayed = |from: &str, to: &str| {
            let mut c = car(review("ready", released(H1)));
            c["metadata"] = json!({ crate::train::dock_regate::BASE_REGATE: {
                "main": "m", "base": "b", "head": to, "from": from }});
            c
        };
        let never = || -> Judgement { panic!("a car with a release is not judged") };
        assert_eq!(
            dock(&replayed(H1, H2), H2, never, |a, b| {
                assert_eq!((a, b), (H1, H2));
                Ok("pid".into())
            }),
            Dock::Check {
                review: REVIEWER.into(),
                reviewed: H1.into(),
                carry: Some("pid".into()),
            },
            "the review is checked against the head it READ"
        );
        let (changed, bind) = hold(dock(&replayed(H1, H2), H2, never, |_, _| {
            Err("its own diff changed in the replay".into())
        }));
        assert!(changed.contains("its own diff changed"), "{changed}");
        assert!(bind);
        // A replay from another head, or to another head, carries nothing.
        for c in [replayed(H2, H2), replayed(H1, "3".repeat(40).as_str())] {
            let (why, _) = hold(dock(&c, H2, never, unasked));
            assert!(why.contains("no dock replay"), "{why}");
        }
    }

    /// THE RECORD IS A LABEL (re-review N1). One step-metadata PATCH can
    /// write a release AT the new head naming a review of an old one
    /// (`reviewed_sha`). Before this, the dock took that on the record's
    /// word and returned `Check` with no patch-id asked — any content at
    /// H2 boarded on a review of H1. Now the car's own diff must match
    /// the head the review read, on every pass.
    #[test]
    fn a_forged_reviewed_sha_at_the_head_is_proved_not_believed() {
        let forged = || {
            let mut md = released(H2);
            md[boss_jobs::car::RELEASE]["reviewed_sha"] = json!(H1);
            car(review("ready", md))
        };
        let never = || -> Judgement { panic!("a car with a release is not judged") };
        let (why, bind) = hold(dock(&forged(), H2, never, |from, to| {
            assert_eq!(
                (from, to),
                (H1, H2),
                "the head the review READ, against this one"
            );
            Err("its own diff changed".into())
        }));
        assert!(why.contains("names a review of 111111111111"), "{why}");
        assert!(bind);
        // The same record over a change that really is unchanged (a carry
        // the conductor recorded) still boards, re-proved every pass.
        assert_eq!(
            dock(&forged(), H2, never, |_, _| Ok("pid".into())),
            Dock::Check {
                review: REVIEWER.into(),
                reviewed: H1.into(),
                carry: None,
            }
        );
    }

    #[test]
    fn the_dock_hold_records_the_head_it_judged_in_the_same_write() {
        let (path, body) =
            review_hold_write(&car(review("ready", json!({}))), "touches x", Some(H1))
                .expect("an open review takes the hold")
                .expect("an unheld car owes the write");
        assert_eq!(
            path,
            "/api/jobs/c0ffee00-0000-0000-0000-000000000000/steps/5e5e5e5e/metadata"
        );
        assert_eq!(body, json!({"hold": "touches x", HOLD_SHA: H1}));
        assert_eq!(
            review_hold_write(
                &car(review("ready", json!({"hold": "area policy"}))),
                "r",
                Some(H1)
            ),
            Ok(None),
            "a car held already keeps its reason"
        );
        assert!(review_hold_write(&car(review("completed", json!({}))), "r", Some(H1)).is_err());
    }

    /// Two heads carrying one change onto different bases share a
    /// patch-id; a head whose change differs does not.
    #[test]
    fn same_change_reads_the_cars_own_diff_on_both_heads() {
        let (root, g, w) = repo("mutating-verb-same-change");
        g(&["init", "-q", "-b", "main"]);
        w("a.txt", "one\n");
        w("b.txt", "one\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "base"]);
        let base = g(&["rev-parse", "HEAD"]);
        g(&["checkout", "-q", "-b", "car", &base]);
        w("a.txt", "one\ntwo\n");
        g(&["commit", "-qam", "car"]);
        let h1 = g(&["rev-parse", "HEAD"]);
        g(&["checkout", "-q", "main"]);
        w("b.txt", "one\nmain\n");
        g(&["commit", "-qam", "main moves"]);
        // The dock's replay: the car's commit onto the new main.
        g(&["checkout", "-q", "-b", "replayed", "main"]);
        g(&["cherry-pick", &h1]);
        let h2 = g(&["rev-parse", "HEAD"]);
        // A replay that also changed the car's own file.
        g(&["checkout", "-q", "-b", "edited", "replayed"]);
        w("a.txt", "one\ntwo\nthree\n");
        g(&["commit", "-qam", "more"]);
        let h3 = g(&["rev-parse", "HEAD"]);

        let same = same_change(&root, "main", &h1, &h2).expect("one change on two bases");
        assert!(!same.is_empty());
        let differs = same_change(&root, "main", &h1, &h3).expect_err("the change moved");
        assert!(differs.contains("patch-id"), "{differs}");
        assert!(same_change(&root, "main", &h1, "no-such-rev").is_err());

        // WHITESPACE IS MEANING (re-review N2): the reviewer's measured
        // shape — a YAML re-indent that moves `c:` out from under `b:`.
        // `--stable` gave both heads one id; `--verbatim` must not.
        let yaml = |name: &str, body: &str| -> String {
            g(&["checkout", "-q", "-B", name, &base]);
            w("c.yaml", body);
            g(&["add", "."]);
            g(&["commit", "-qm", name]);
            g(&["rev-parse", "HEAD"])
        };
        let nested = yaml("nested", "a:\n  b:\n    c: 1\n");
        let moved = yaml("moved", "a:\n  b:\n  c: 1\n");
        g(&["checkout", "-q", "main"]);
        assert!(
            same_change(&root, "main", &nested, &moved).is_err(),
            "a re-indent that changes the YAML's meaning is not the same change"
        );
    }

    #[test]
    fn park_refuses_only_what_the_predicate_holds_and_names_the_doors_that_hold() {
        assert_eq!(park_refusal("feat/x", &Judgement::Clear), None);
        let why = park_refusal("feat/x", &touches(&["infra/forge/x.sh"], &[])).expect("refused");
        for want in [
            "REFUSED",
            "infra/forge/x.sh",
            "--park-",
            "rerail",
            "--finish",
        ] {
            assert!(why.contains(want), "{want} in {why}");
        }
        assert!(park_refusal("feat/x", &Judgement::Unread("x".into())).is_some());
    }

    #[test]
    fn a_verb_is_mutating_by_its_word_or_its_approval() {
        assert!(is_mutating(
            &json!({ "about": "MUTATING — deletes a thing" })
        ));
        assert!(is_mutating(
            &json!({ "about": "deletes a thing", "requires_approval": true })
        ));
        assert!(!is_mutating(&json!({ "about": "READ-ONLY — df -h" })));
        assert!(!is_mutating(
            &json!({ "about": "reads", "requires_approval": false })
        ));
        assert!(!is_mutating(&json!({})));
    }

    /// The four escapes the review of 0243fde5 found, each covered: the
    /// runner and what it sources; a helper a script sources, and one
    /// that helper calls; a script behind an interpreter; and a verb
    /// whose label says it only reads.
    #[test]
    fn every_verb_what_it_runs_and_the_runner_are_covered_whatever_the_label() {
        let tree = side(&[
            (
                "infra/ops/verbs/reap.json",
                &verb("MUTATING — reaps", &["infra/forge/reap.sh", "{1}"]),
            ),
            (
                "infra/ops/verbs/df.json",
                &verb("READ-ONLY — df", &["bash", "infra/forge/df.sh"]),
            ),
            (
                "infra/ops/ops-runner.sh",
                "#!/usr/bin/env bash\n. \"$(dirname \"$0\")/../lib/jq.sh\"\n",
            ),
            ("infra/lib/jq.sh", "JQ=jq\n"),
            ("infra/lib/unused.sh", "true\n"),
            (
                "infra/forge/reap.sh",
                "#!/usr/bin/env bash\n. \"$(dirname \"$0\")/reap.judge.sh\"\n",
            ),
            (
                "infra/forge/reap.judge.sh",
                "bash \"$(dirname \"$0\")/list-disks\"\n",
            ),
            ("infra/forge/list-disks", "#!/bin/sh\nlsblk\n"),
            ("infra/forge/df.sh", "df -h\n"),
            ("infra/forge/other.sh", "true\n"),
            ("infra/forge/NOTES.md", "see other.sh\n"),
        ]);
        let changed = paths(&[
            "crates/core/boss-jobs/src/lib.rs",
            "infra/forge/NOTES.md",
            "infra/forge/df.sh",
            "infra/forge/list-disks",
            "infra/forge/other.sh",
            "infra/forge/reap.judge.sh",
            "infra/forge/reap.sh",
            "infra/lib/jq.sh",
            "infra/lib/unused.sh",
            "infra/ops/ops-runner.sh",
            "infra/ops/verbs/df.json",
            "infra/ops/verbs/reap.json",
        ]);
        assert_eq!(
            touched(&changed, &[&tree]),
            touches(
                &[
                    "infra/forge/df.sh",
                    "infra/forge/list-disks",
                    "infra/forge/reap.judge.sh",
                    "infra/forge/reap.sh",
                    "infra/lib/jq.sh",
                    "infra/ops/ops-runner.sh",
                    "infra/ops/verbs/df.json",
                    "infra/ops/verbs/reap.json",
                ],
                &["infra/forge/reap.sh", "infra/ops/verbs/reap.json"],
            )
        );
        // A read-only verb's script, alone, holds: the label is the
        // thing under review, so it cannot decide the hold.
        assert_eq!(
            touched(&paths(&["infra/forge/df.sh"]), &[&tree]),
            touches(&["infra/forge/df.sh"], &[])
        );
        // A markdown file's prose is not followed; a file nothing names is clear.
        assert_eq!(
            touched(&paths(&["infra/forge/other.sh"]), &[&tree]),
            Judgement::Clear
        );
    }

    /// A module file of the boss CLI, by name under [`CLI_SRC`].
    fn cli(rel: &str) -> String {
        format!("{CLI_SRC}{rel}")
    }

    /// THE BOSS CLI A VERB RUNS IS COVERED (backlog b089661d). Each verb
    /// and script in the shape it has on main: `reach` and
    /// `run-car-probe` put `boss` at `argv[0]`; publish-workflow.sh runs
    /// `"$BOSS_BIN" workflow publish`; merge-tenant-main.sh runs
    /// `"$CLI" tenant check` inside a command substitution; the converge's
    /// cluster-deploy-lib.sh runs `"$cli" tenant check` inside another.
    /// What each reaches is covered — `src/<sub>.rs`, `src/<sub>/`, and
    /// the in-crate modules those name one level down — and nothing past
    /// that level, no hub, and no module nothing runs.
    #[test]
    fn a_boss_subcommand_a_verb_or_its_script_runs_covers_its_module_one_level_down() {
        let tree = side(&[
            (
                "infra/ops/verbs/reach.json",
                &verb("reaches a host", &["boss", "reach", "{1}", "{2}"]),
            ),
            (
                "infra/ops/verbs/run-car-probe.json",
                &verb(
                    "runs a car's probe",
                    &["boss", "prove", "{1}", "--from-car", "--unattended"],
                ),
            ),
            (
                "infra/ops/verbs/publish-workflow.json",
                &verb(
                    "MUTATING — publishes",
                    &["infra/gcp/publish-workflow.sh", "{1}", "{2}"],
                ),
            ),
            (
                "infra/ops/verbs/merge-tenant-main.json",
                &verb(
                    "MUTATING — merges",
                    &["infra/forge/merge-tenant-main.sh", "{1}", "{2}", "{3}"],
                ),
            ),
            (
                "infra/ops/verbs/converge.json",
                &verb("MUTATING — converges", &["infra/forge/converge-now.sh"]),
            ),
            (
                "infra/gcp/publish-workflow.sh",
                "#!/usr/bin/env bash\nBOSS_BIN=\"${BOSS_BIN:-}\"\n\
                 if ! \"$BOSS_BIN\" workflow publish \"$KIND\" \"$KIND_FILE\" --dry-run > \"$TMP/lint.out\" 2>&1; then\n",
            ),
            (
                "infra/forge/merge-tenant-main.sh",
                "#!/usr/bin/env bash\nCLI=\"${BOSS_CLI:-boss}\"\n\
                 check=$(cd \"$WORK/tree\" && \"$CLI\" tenant check . 2>&1) || check_rc=$?\n",
            ),
            (
                "infra/forge/converge-now.sh",
                "#!/usr/bin/env bash\n. \"$(dirname \"$0\")/cluster-deploy-lib.sh\"\n",
            ),
            (
                "infra/forge/cluster-deploy-lib.sh",
                "tenant_check() {\n    local src=\"$1\" cli=\"${BOSS_CLI:-boss}\"\n\
                 \x20   out=$(\"$cli\" tenant check \"$src\" 2>&1) || rc=$?\n}\n",
            ),
            (&cli("main.rs"), "mod reach;\nmod prove;\nmod workflow;\n"),
            (&cli("reach.rs"), "pub fn run() {}\n"),
            (&cli("prove.rs"), "use crate::freshness;\n"),
            (&cli("freshness.rs"), "use crate::far;\n"),
            (&cli("far.rs"), "pub fn far() {}\n"),
            (&cli("workflow.rs"), "fn f() { identity::actor(); }\n"),
            (&cli("workflow/lint.rs"), "pub fn lint() {}\n"),
            (&cli("identity.rs"), "pub fn actor() {}\n"),
            (
                &cli("tenant.rs"),
                "use crate::tenant_export;\nfn g() { crate::gate::run(); train::go(); }\n",
            ),
            (&cli("tenant_export.rs"), "pub fn export() {}\n"),
            (&cli("gate.rs"), "pub fn run() {}\n"),
            (&cli("train/mod.rs"), "pub fn go() {}\n"),
            (&cli("job.rs"), "pub fn file() {}\n"),
        ]);
        let every_cli: Vec<String> = tree
            .keys()
            .filter(|p| p.starts_with(CLI_SRC))
            .cloned()
            .collect();
        let Judgement::Touches { paths: held, .. } = touched(&every_cli, &[&tree]) else {
            panic!("the CLI a verb runs holds");
        };
        assert_eq!(
            held,
            paths(&[
                &cli("freshness.rs"),
                &cli("identity.rs"),
                &cli("prove.rs"),
                &cli("reach.rs"),
                &cli("tenant.rs"),
                &cli("tenant_export.rs"),
                &cli("workflow.rs"),
                &cli("workflow/lint.rs"),
            ]),
            "far.rs is two levels down, gate and train are hubs, job and main nothing runs"
        );
        // Each shape alone: the one script that names the sub is what
        // reaches it, so a matcher keyed on boss or $BOSS_BIN only would
        // miss both tenant-check sites.
        for (script, sub) in [
            ("infra/gcp/publish-workflow.sh", "workflow.rs"),
            ("infra/forge/merge-tenant-main.sh", "tenant.rs"),
            ("infra/forge/cluster-deploy-lib.sh", "tenant.rs"),
        ] {
            let mut alone = tree.clone();
            for other in [
                "infra/gcp/publish-workflow.sh",
                "infra/forge/merge-tenant-main.sh",
                "infra/forge/cluster-deploy-lib.sh",
            ] {
                if other != script {
                    alone.insert(other.to_string(), "true\n".to_string());
                }
            }
            assert!(
                covered(&alone).contains(&cli(sub)),
                "{script} reaches {sub}"
            );
        }
        // An expansion that is not the binary, and a comment, run no
        // subcommand: `"$DOCKER" inspect` (forge-backup.sh) and a
        // `# boss job file` line leave inspect.rs and job.rs clear.
        let mut other = tree.clone();
        other.insert(
            "infra/forge/converge-now.sh".into(),
            "#!/usr/bin/env bash\nDOCKER=docker\nrunning=$(\"$DOCKER\" inspect -f x)\n\
             # then boss job file records it\n. ./cluster-deploy-lib.sh\n"
                .into(),
        );
        other.insert(cli("inspect.rs"), "pub fn run() {}\n".into());
        let c = covered(&other);
        assert!(
            c.contains(&cli("tenant.rs")),
            "the helper still runs tenant check"
        );
        assert!(
            !c.contains(&cli("inspect.rs")) && !c.contains(&cli("job.rs")),
            "{c:?}"
        );
        // A CLI file is reached by what runs it, never by its basename:
        // a script naming main.rs does not cover the crate root.
        let mut named = tree.clone();
        named.insert(
            "infra/forge/converge-now.sh".into(),
            "#!/usr/bin/env bash\n# built from main.rs and job.rs\n".into(),
        );
        let c = covered(&named);
        assert!(
            !c.contains(&cli("main.rs")) && !c.contains(&cli("job.rs")),
            "{c:?}"
        );
    }

    /// THE HUBS ARE A DECISION, SO THEY ARE NAMED AND PINNED (backlog
    /// b089661d triage): `gate` and `train` are the CLI's shared
    /// plumbing, and following them one level took the held share of 30
    /// days' commits from ~19% to 37%+. A module that is a hub is not
    /// covered by reference, nor by a script that runs it by name.
    #[test]
    fn the_cli_hubs_are_gate_and_train_and_no_rule_covers_them() {
        assert_eq!(CLI_HUBS, ["gate", "train"]);
        let tree = side(&[
            (
                "infra/ops/verbs/x.json",
                &verb("MUTATING — x", &["infra/forge/x.sh"]),
            ),
            (
                "infra/forge/x.sh",
                "#!/usr/bin/env bash\nboss gate \"$b\" --wait\n/usr/local/bin/boss train run\nboss car-retire \"$c\"\n",
            ),
            (&cli("gate.rs"), "pub fn run() {}\n"),
            (&cli("train/mod.rs"), "pub fn go() {}\n"),
            (&cli("car_retire.rs"), "fn f() { gate::run(); }\n"),
        ]);
        let c = covered(&tree);
        assert!(
            c.contains(&cli("car_retire.rs")),
            "a hyphened sub is its module: {c:?}"
        );
        assert!(
            !c.contains(&cli("gate.rs")) && !c.contains(&cli("train/mod.rs")),
            "{c:?}"
        );
    }

    /// A FILE THAT SAYS IT IS HELD IS HELD (backlog 275d75af): the
    /// claim, wrapped across comment lines as a header wraps it, covers
    /// the file and the CLI module its header names as `boss <sub>` —
    /// and nothing else in the crate. Read on BOTH sides, so a car that
    /// deletes the sentence is held by the base that still says it.
    #[test]
    fn a_file_whose_header_claims_a_review_is_held_with_the_renderer_it_names() {
        let header = "# THE SHEET. `boss recovery sheet` reads this file.\n\
                      # No regex can refuse it. Every change to this file is a\n\
                      # credentials-area car and waits for an adversarial review that reads\n\
                      # each line. Then `boss prove` the road.\n[[road]]\n";
        let tree = |sheet: &str| {
            side(&[
                ("infra/ops/verbs/df.json", &verb("READ-ONLY", &["df"])),
                ("infra/recovery/sheet.toml", sheet),
                ("infra/estate/estate.toml", "[node]\n"),
                (&cli("recovery.rs"), "mod pdf;\n"),
                (&cli("recovery/pdf.rs"), "fn x() {}\n"),
                (&cli("other.rs"), "fn y() {}\n"),
                (&cli("prove.rs"), "fn z() {}\n"),
            ])
        };
        let (claims, silent) = (tree(header), tree("[[road]]\n"));
        for held in [
            "infra/recovery/sheet.toml",
            &cli("recovery.rs"),
            &cli("recovery/pdf.rs"),
        ] {
            let judged = touched(&paths(&[held]), &[&claims]);
            assert_eq!(judged, touches(&[held], &[]), "{held}");
            let reason = judged.hold_reason().expect("a claimant holds");
            assert!(reason.contains("claims a review"), "{reason}");
        }
        for clear in [
            "infra/estate/estate.toml",
            &cli("other.rs"),
            &cli("prove.rs"),
        ] {
            assert_eq!(
                touched(&paths(&[clear]), &[&claims]),
                Judgement::Clear,
                "{clear}"
            );
        }
        assert_eq!(
            touched(&paths(&["infra/recovery/sheet.toml"]), &[&silent]),
            Judgement::Clear,
            "no claim, no hold: the sentence is the key"
        );
        assert_eq!(
            touched(&paths(&["infra/recovery/sheet.toml"]), &[&claims, &silent]),
            touches(&["infra/recovery/sheet.toml"], &[]),
            "deleting the claim is held by the base that still makes it"
        );
    }

    /// THE REAL TREE: the printed re-entry sheet's header says every
    /// change to it waits for an adversarial review, and the hold list
    /// is that sentence — so the sheet, every file beside it, and the
    /// renderer `boss recovery sheet` are held, and the files the sheet
    /// only READS facts from are not (backlog 275d75af; CLAUDE.md 9a).
    /// Reword the header and this fails, rather than the sheet boarding
    /// unheld in silence.
    #[test]
    fn on_the_real_tree_the_re_entry_sheet_and_its_renderer_are_held() {
        let tree = real_tree();
        let c = covered(&tree);
        let beside: Vec<&String> = tree
            .keys()
            .filter(|p| p.starts_with("infra/recovery/"))
            .collect();
        assert!(
            beside.iter().any(|p| *p == "infra/recovery/re-entry.toml"),
            "{beside:?}"
        );
        for p in beside {
            assert!(c.contains(p), "{p} is under infra/recovery/ and not held");
        }
        for p in [cli("recovery.rs"), cli("recovery/pdf.rs")] {
            assert!(c.contains(&p), "{p} renders the sheet");
        }
        // The claim adds the sheet, its renderer and the files it prints
        // prose from, and nothing else: a fact source is held only if
        // another rule holds it already.
        let Claimed {
            claimants,
            renderers,
            prose,
        } = claimed(&tree);
        assert_eq!(
            claimants.iter().map(String::as_str).collect::<Vec<_>>(),
            [
                "infra/cluster/manifests/boss-node-maintenance-admission.yaml",
                "infra/cluster/manifests/boss-node-maintenance.yaml",
                "infra/dev/dev-session.sh",
                "infra/recovery/re-entry.toml"
            ],
            "the dev pod's login selector makes the claim too (review a27c860d F4), and so \
             does the admission policy that bounds the node-maintenance namespace (backlog \
             8eac4893): it is the bound on a privileged namespace, and a car loosening it \
             alone would otherwise park unheld; and so does the one manifest whose pod \
             holds a node capability (review 9b05cc55 N8)"
        );
        assert_eq!(
            renderers.iter().map(String::as_str).collect::<Vec<_>>(),
            ["recovery"]
        );
        assert_eq!(
            prose.iter().map(String::as_str).collect::<Vec<_>>(),
            ["infra/estate/dev-door.json"],
            "derived from the sheet's own `fields` lines (backlog 27272837)"
        );
    }

    /// A FILE THE SHEET PRINTS SENTENCES FROM IS HELD WITH IT (backlog
    /// 27272837, review 818ac256 F1): a `[[road.line]]` that names
    /// `fields` prints a list of records field by field, and its `file`
    /// is held — even outside [`CANDIDATES`], since the hold list is
    /// paths, not the side's files. A line reading ONE value by `key`
    /// or `pattern` is not. Read on both sides, so a car that drops the
    /// `fields` line is held by the base that still prints from it.
    #[test]
    fn a_file_the_sheet_prints_prose_from_is_held() {
        let sheet = |door_line: &str| {
            format!(
                "# `boss recovery sheet` reads this file. Every change to this file is a\n\
                 # credentials-area car and waits for an adversarial review.\n\
                 [[road]]\nid = \"door\"\ntitle = \"The door\"\n\
                 [[road.line]]\nlabel = \"Host\"\nfile = \"apps/x/door.json\"\nkey = \"host\"\n\
                 [[road.line]]\nlabel = \"Address\"\nfile = \"infra/estate/estate.toml\"\nkey = \"a\"\n\
                 {door_line}"
            )
        };
        let steps = "[[road.line]]\nlabel = \"Steps\"\nfile = \"apps/x/door.json\"\n\
                     key = \"steps\"\nfields = [\"what\", \"why\"]\n";
        let tree = |text: &str| {
            side(&[
                ("infra/ops/verbs/df.json", &verb("READ-ONLY", &["df"])),
                ("infra/recovery/sheet.toml", text),
                ("infra/estate/estate.toml", "a = 1\n"),
                (&cli("recovery.rs"), "fn x() {}\n"),
            ])
        };
        let (prints, values_only) = (tree(&sheet(steps)), tree(&sheet("")));
        let door = paths(&["apps/x/door.json"]);
        let judged = touched(&door, &[&prints]);
        assert_eq!(judged, touches(&["apps/x/door.json"], &[]));
        let reason = judged.hold_reason().expect("a prose source holds");
        assert!(reason.contains("prints prose from"), "{reason}");
        assert_eq!(
            touched(&paths(&["infra/estate/estate.toml"]), &[&prints]),
            Judgement::Clear,
            "a file read for one value is not held"
        );
        assert_eq!(
            touched(&door, &[&values_only]),
            Judgement::Clear,
            "a file the sheet reads only a value from is not held"
        );
        assert_eq!(
            touched(&door, &[&prints, &values_only]),
            touches(&["apps/x/door.json"], &[]),
            "dropping the fields line is held by the base that still prints it"
        );
        let unclaimed = tree(&sheet(steps).replace("Every change", "A change"));
        assert_eq!(
            touched(&door, &[&unclaimed]),
            Judgement::Clear,
            "only a file that claims a review lends its hold"
        );
    }

    /// THE REAL TREE: a change to the dev-door JSON ALONE is held — the
    /// car review 818ac256 F1 said would board unheld (backlog 27272837).
    #[test]
    fn on_the_real_tree_a_change_to_the_dev_door_steps_alone_is_held() {
        let tree = real_tree();
        let judged = touched(&paths(&["infra/estate/dev-door.json"]), &[&tree]);
        assert_eq!(judged, touches(&["infra/estate/dev-door.json"], &[]));
    }

    /// THE REAL TREE: a change to the dev pod's login selector ALONE is
    /// held (review a27c860d F4, backlog e2d63c28). Every interactive ssh
    /// login runs it as root, and when the menu left boss-dev.yaml for
    /// `infra/dev/` an edit to it stopped drawing that manifest's hold —
    /// so its header makes the [`CLAIM`], and the sentence is the list.
    #[test]
    fn on_the_real_tree_a_change_to_the_login_selector_alone_is_held() {
        let tree = real_tree();
        let judged = touched(&paths(&["infra/dev/dev-session.sh"]), &[&tree]);
        assert_eq!(judged, touches(&["infra/dev/dev-session.sh"], &[]));
        let reason = judged.hold_reason().expect("the selector is held");
        assert!(reason.contains("claims a review"), "{reason}");
    }

    /// HOW PROSE IS TOLD FROM A VALUE, pinned on the real sheet (backlog
    /// 27272837). The rule is the line's shape: `fields` prints records
    /// and is held; every other read — a line's `key` or `pattern`, a
    /// command's `with`, a road's `needs` — reads ONE value and is not.
    /// That holds only while every such read resolves to a value (an
    /// address, a port, a tag, a list of them), and a sentence read from
    /// an unheld file is the hole this item closed. So every single read
    /// is resolved here by the renderer's own resolver, and any element
    /// that is a sentence ([`crate::recovery::reads_as_a_sentence`]) fails, naming the file and
    /// the key, rather than the file printing prose unheld.
    #[test]
    fn on_the_real_tree_every_read_the_hold_calls_a_value_is_a_value() {
        use crate::recovery::{self, Line, LineOut, Ref, Road, Sheet, Value};
        let root = boss_testing::repo_root();
        let text = std::fs::read_to_string(root.join(recovery::SOURCE)).expect("the sheet");
        let real = recovery::parse_sheet(&text).expect("the sheet parses");
        let single = |r: &Ref| Line {
            prose: None,
            not_tree_held: None,
            label: Some(format!("{}#{:?}{:?}", r.file, r.key, r.pattern)),
            file: Some(r.file.clone()),
            key: r.key.clone(),
            pattern: r.pattern.clone(),
            fields: None,
            command: None,
            with: None,
        };
        let mut reads: Vec<Line> = Vec::new();
        for road in &real.road {
            for line in &road.line {
                if let Some(file) = line.file.as_ref().filter(|_| line.fields.is_none()) {
                    reads.push(single(&Ref {
                        file: file.clone(),
                        key: line.key.clone(),
                        pattern: line.pattern.clone(),
                    }));
                }
                reads.extend(line.with.iter().flat_map(|w| w.values()).map(single));
            }
            if let Some(n) = &road.needs {
                reads.extend(n.host.iter().chain(n.path.iter()).map(single));
            }
        }
        assert!(reads.len() > 40, "{} single reads", reads.len());
        let one = Sheet {
            road: vec![Road {
                id: "reads".into(),
                title: "reads".into(),
                proven_by: None,
                needs: None,
                line: reads,
            }],
        };
        let rendered = recovery::resolve(&root, &one).expect("every single read resolves");
        let sentences: Vec<String> = rendered.roads[0]
            .lines
            .iter()
            .filter_map(|l| match l {
                LineOut::Fact {
                    label,
                    value: Value::Text(t),
                    ..
                } => Some((label, t)),
                _ => None,
            })
            .filter(|(_, t)| t.split(", ").any(recovery::reads_as_a_sentence))
            .map(|(label, t)| format!("{label} = {t:?}"))
            .collect();
        assert!(
            sentences.is_empty(),
            "a read the hold calls a VALUE resolves to a sentence — print it through a \
             `fields` line so its file is held, or hold it another way: {sentences:#?}"
        );
    }

    fn real_tree() -> Side {
        fn walk(root: &Path, dir: &Path, out: &mut Side) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(root, &p, out);
                } else if let (Ok(rel), Ok(bytes)) = (p.strip_prefix(root), std::fs::read(&p)) {
                    out.insert(
                        rel.to_string_lossy().into_owned(),
                        String::from_utf8_lossy(&bytes).into_owned(),
                    );
                }
            }
        }
        let root = boss_testing::repo_root();
        let mut tree = Side::new();
        for dir in CANDIDATES.iter().chain([&CLI_SRC]) {
            walk(&root, &root.join(dir), &mut tree);
        }
        tree
    }

    /// THE REAL TREE, read off disk: every module the packet names as
    /// run by a verb is covered — `workflow`, `tenant`, `prove`, `reach`
    /// — and the hubs and the crate root are not. The module names come
    /// from the listing, so a fixture that invented them could pass
    /// while the tree does not.
    #[test]
    fn on_the_real_tree_the_cli_modules_the_mutating_verbs_run_are_covered() {
        let tree = real_tree();
        let c = covered(&tree);
        for want in ["workflow.rs", "tenant.rs", "prove.rs", "reach.rs"] {
            assert!(c.contains(&cli(want)), "{want} is run by a verb");
        }
        for not in ["gate.rs", "train/mod.rs", "main.rs"] {
            assert!(!c.contains(&cli(not)), "{not} is not covered");
        }
        // THE BOUND: 31 of 88 files on 2026-09-28. Reading any `$X` as
        // the binary took it to 47 (`"$DOCKER" inspect`, `$CAR_KIND
        // car`), and comments to 59; past half the crate, every CLI car
        // waits, and a check that holds everything holds nothing.
        let all: Vec<&String> = tree.keys().filter(|p| p.starts_with(CLI_SRC)).collect();
        let held: Vec<&String> = c.iter().filter(|p| p.starts_with(CLI_SRC)).collect();
        assert!(
            held.len() * 2 < all.len(),
            "{} of {} CLI files covered: {held:?}",
            held.len(),
            all.len()
        );
    }

    #[test]
    fn either_side_of_the_change_names_what_a_verb_ran() {
        // The car repoints the verb: only the base still names x.sh.
        let base = side(&[
            (
                "infra/ops/verbs/x.json",
                &verb("MUTATING — deletes", &["infra/forge/x.sh"]),
            ),
            ("infra/forge/x.sh", "rm\n"),
        ]);
        let tip = side(&[
            (
                "infra/ops/verbs/x.json",
                &verb("READ-ONLY — lists", &["infra/forge/y.sh"]),
            ),
            ("infra/forge/x.sh", "rm -rf\n"),
            ("infra/forge/y.sh", "ls\n"),
        ]);
        let changed = paths(&["infra/forge/x.sh"]);
        assert_eq!(
            touched(&changed, &[&tip]),
            Judgement::Clear,
            "the tip alone"
        );
        assert_eq!(
            touched(&changed, &[&base, &tip]),
            touches(&["infra/forge/x.sh"], &["infra/forge/x.sh"])
        );
    }

    #[test]
    fn an_unparseable_verb_holds_and_reads_as_mutating() {
        let tree = side(&[
            ("infra/ops/verbs/bad.json", "{ not json"),
            ("infra/ops/verbs/ok.json", &verb("READ-ONLY", &["df"])),
        ]);
        assert_eq!(
            touched(&paths(&["infra/ops/verbs/bad.json"]), &[&tree]),
            touches(&["infra/ops/verbs/bad.json"], &["infra/ops/verbs/bad.json"])
        );
    }

    #[test]
    fn no_verb_on_either_side_is_the_registry_moved_never_clear() {
        let moved = side(&[("infra/forge/x.sh", "rm\n")]);
        let judged = touched(&paths(&["README.md"]), &[&moved, &moved]);
        assert!(
            matches!(&judged, Judgement::Unread(why) if why.contains("registry moved")),
            "{judged:?}"
        );
    }

    #[test]
    fn only_a_clear_judgement_boards_on_its_green_alone() {
        assert_eq!(Judgement::Clear.hold_reason(), None);
        let held = touches(
            &["infra/ops/verbs/x.json", "infra/forge/x.sh"],
            &["infra/ops/verbs/x.json"],
        )
        .hold_reason()
        .expect("a touch holds");
        for want in [
            "infra/ops/verbs/x.json",
            "infra/forge/x.sh",
            "MUTATING: infra/ops/verbs/x.json",
            "adversarial review",
            "boss release",
            "RELEASE",
            "fdbb447e",
        ] {
            assert!(held.contains(want), "{want} in {held}");
        }
        let unlabelled = touches(&["infra/forge/df.sh"], &[])
            .hold_reason()
            .expect("a read-only verb's script holds too");
        assert!(!unlabelled.contains("MUTATING"), "{unlabelled}");
        let unread = Judgement::Unread("git diff: bad revision".into())
            .hold_reason()
            .expect("no evidence is not a pass");
        assert!(unread.contains("git diff: bad revision"), "{unread}");
        for r in [held, unlabelled, unread] {
            assert!(!r.contains('\'') && !r.contains('`'), "shell-plain: {r}");
        }
    }

    #[test]
    fn the_gate_holds_a_park_on_the_predicate_and_appends_to_a_hold_it_was_given() {
        let judged = touches(&["infra/forge/x.sh"], &[]);
        let reason = judged.hold_reason().expect("held");
        assert_eq!(gate_hold(None, true, &judged), Some(reason.clone()));
        assert_eq!(
            gate_hold(Some("area policy".into()), true, &judged),
            Some(format!("area policy; {reason}")),
            "a hold a person wrote stands, and the diff's reason rides beside it"
        );
        assert_eq!(
            gate_hold(Some("area policy".into()), true, &Judgement::Clear).as_deref(),
            Some("area policy")
        );
        assert_eq!(gate_hold(None, false, &judged), None, "no car, no hold");
        assert_eq!(gate_hold(None, true, &Judgement::Clear), None);
        assert!(gate_hold(None, true, &Judgement::Unread("x".into())).is_some());
    }

    /// EVERY DOOR TO THE DOCK READS THE ONE PREDICATE, AND BEFORE IT
    /// WRITES. Read off the three verbs' own source, where the order
    /// lives: the gate judges after its sha and base are final (a
    /// `--rebase` moves both) and before the hold is stamped; the
    /// rerail holds before the receipt makes the car boardable; the
    /// park refuses before it reads the dock to file or refresh.
    #[test]
    fn every_door_to_the_dock_judges_the_diff_before_it_writes() {
        let order = |src: &str, fn_start: &str, first: &str, then: &[&str]| {
            let body = &src[src.find(fn_start).expect(fn_start)..];
            let at = |needle: &str| {
                body.find(needle)
                    .unwrap_or_else(|| panic!("{needle:?} is not after {fn_start:?}"))
            };
            let judged = at(first);
            for later in then {
                assert!(judged < at(later), "{first} precedes {later} in {fn_start}");
            }
        };
        order(
            include_str!("gate.rs"),
            "pub async fn run(",
            "crate::mutating_verb::judge(Path::new(\".\"), &base_obs.base, &sha)",
            &[
                "boss_jobs::car::HOLD_UNBOUND: (!hold_bound).then_some(true)",
                "reqwest::Method::POST, \"/api/jobs\", Some(body)",
            ],
        );
        let gate = include_str!("gate.rs");
        let run = &gate[gate.find("pub async fn run(").expect("run")..];
        assert!(
            run.find("sha = done.new_head.clone()")
                .expect("the rebase moves the sha")
                < run.find("crate::mutating_verb::judge(").expect("judged"),
            "the gate judges the head it gates, after --rebase"
        );
        order(
            include_str!("rerail.rs"),
            "async fn finish(",
            "crate::mutating_verb::review_hold_write(car, &reason, bound)",
            // The call as a statement of `finish`'s body, however rustfmt
            // wraps its arguments (the proof intent lengthened it, 79a17c7a).
            &["\n    repoint(\n"],
        );
        order(
            include_str!("park.rs"),
            "pub(crate) async fn run(",
            "crate::mutating_verb::park_refusal(branch, &judged)",
            &["all_open_cars(&http)", "reqwest::Method::POST"],
        );
        // The conductor judges the head the fork carries, before the
        // receipt check can launch a dock re-gate and before the car is
        // added to what boards (backlog b7b02024 car 1).
        order(
            include_str!("train/conductor.rs"),
            "async fn candidates(",
            "let boards = fork_head(&self.cfg.clone, &branch)?;",
            &["self.review_hold(&j, &jid, boards.as_deref())"],
        );
        order(
            include_str!("train/conductor.rs"),
            "async fn candidates(",
            "self.review_hold(&j, &jid, boards.as_deref())",
            &[
                "receipt_skip_reason(&j, boards.as_deref())",
                "out.push((j, branch));",
            ],
        );
    }

    /// The conductor's reader against a REAL repository: the diff is the
    /// car's own change from its merge-base with main, so a car that
    /// touches nothing covered is clear even after MAIN changed a verb's
    /// script, and a car that touches one holds; a main or a head git
    /// cannot resolve is unread.
    #[test]
    fn judge_on_main_reads_the_cars_own_change_from_its_base() {
        let (root, g, w) = repo("mutating-verb-judge-on-main");
        g(&["init", "-q", "-b", "main"]);
        w(
            "infra/ops/verbs/reap.json",
            r#"{"about":"MUTATING — reaps","hosts":["forge"],"argv":["infra/forge/reap.sh"]}"#,
        );
        w("infra/forge/reap.sh", "#!/usr/bin/env bash\n");
        w("README.md", "one\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "base"]);
        let base = g(&["rev-parse", "HEAD"]);

        g(&["checkout", "-q", "-b", "docs", &base]);
        w("README.md", "two\n");
        g(&["commit", "-qam", "docs"]);
        g(&["checkout", "-q", "-b", "ops", &base]);
        w("infra/forge/reap.sh", "#!/usr/bin/env bash\nrm -rf /x\n");
        g(&["commit", "-qam", "ops"]);
        // Main moves INTO a verb's script after both cars branched.
        g(&["checkout", "-q", "main"]);
        w("infra/forge/reap.sh", "#!/usr/bin/env bash\necho main\n");
        g(&["commit", "-qam", "main moves"]);

        let docs = g(&["rev-parse", "docs"]);
        let ops = g(&["rev-parse", "ops"]);
        assert_eq!(
            judge_on_main(&root, "main", &docs),
            Judgement::Clear,
            "main's change is not the car's"
        );
        assert_eq!(
            judge_on_main(&root, "main", &ops),
            touches(&["infra/forge/reap.sh"], &["infra/forge/reap.sh"])
        );
        let no_main = judge_on_main(&root, "origin/main", &ops);
        assert!(
            matches!(&no_main, Judgement::Unread(why) if why.contains("origin/main")),
            "{no_main:?}"
        );
        assert!(matches!(
            judge_on_main(&root, "main", ""),
            Judgement::Unread(_)
        ));
    }

    /// A scratch git repository and its two helpers.
    fn repo(
        tag: &str,
    ) -> (
        std::path::PathBuf,
        impl Fn(&[&str]) -> String,
        impl Fn(&str, &str),
    ) {
        let root = boss_testing::scratch::scratch_dir(tag);
        let r = root.clone();
        let g = move |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&r)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let w_root = root.clone();
        let w = move |rel: &str, body: &str| {
            let path = w_root.join(rel);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
            boss_testing::scratch::write_file(&path, body)
        };
        (root, g, w)
    }

    /// The reader against a REAL repository, one branch per shape: a
    /// docs change is clear; an edit to a MUTATING verb's script, a
    /// repointed verb (only the BASE names the old script), a deleted
    /// verb (only the base holds it), a script moved without its verb
    /// (only `--no-renames` lists the old path), and a path git quotes
    /// (only `-z` hands it over as itself) each hold; an unresolvable
    /// base is unread — never clear.
    #[test]
    fn judge_reads_the_diff_and_both_trees_from_git() {
        let (root, g, w) = repo("mutating-verb-judge");
        g(&["init", "-q", "-b", "main"]);
        w(
            "infra/ops/verbs/reap.json",
            r#"{"about":"MUTATING — reaps","hosts":["forge"],"argv":["infra/forge/reap.sh"]}"#,
        );
        w(
            "infra/ops/verbs/df.json",
            r#"{"about":"READ-ONLY","hosts":["forge"],"argv":["df","-h"]}"#,
        );
        w(
            "infra/ops/verbs/uni.json",
            r#"{"about":"READ-ONLY","hosts":["forge"],"argv":["infra/forge/é.sh"]}"#,
        );
        w(
            "infra/forge/reap.sh",
            "#!/usr/bin/env bash\nset -uo pipefail\n",
        );
        w("infra/forge/é.sh", "ls\n");
        // The retire-example-reference-rows shape: a MUTATING verb whose
        // script derives its delete set from the example seeds through a
        // helper that names the seed files, never a path under infra/.
        w(
            "infra/ops/verbs/retire.json",
            r#"{"about":"MUTATING — deletes rows","hosts":["forge"],"argv":["infra/forge/retire.sh","{1}"]}"#,
        );
        w(
            "infra/forge/retire.sh",
            "#!/usr/bin/env bash\nDERIVE=\"$(dirname \"$0\")/../postgres/rows.sh\"\n",
        );
        w(
            "infra/postgres/rows.sh",
            "#!/usr/bin/env bash\njq . \"$d/seeds/classes.json\"\n",
        );
        w("examples/brewery/seeds/classes.json", "[]\n");
        w("examples/brewery/seeds/menu.json", "[]\n");
        w(
            "infra/platform/workflows/ops-request.toml",
            "[[workflow]]\n",
        );
        w("README.md", "one\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "base"]);
        let base = g(&["rev-parse", "HEAD"]);
        let branch = |name: &str| g(&["checkout", "-q", "-b", name, &base]);

        branch("clear");
        w("README.md", "two\n");
        g(&["commit", "-qam", "docs"]);
        assert_eq!(judge(&root, &base, "clear"), Judgement::Clear);

        branch("touch");
        w(
            "infra/forge/reap.sh",
            "#!/usr/bin/env bash\nrm -rf /x || true\n",
        );
        g(&["commit", "-qam", "reap more"]);
        assert_eq!(
            judge(&root, &base, "touch"),
            touches(&["infra/forge/reap.sh"], &["infra/forge/reap.sh"])
        );

        branch("repoint");
        w(
            "infra/ops/verbs/reap.json",
            r#"{"about":"READ-ONLY","hosts":["forge"],"argv":["infra/forge/reap2.sh"]}"#,
        );
        w("infra/forge/reap2.sh", "ls\n");
        w("infra/forge/reap.sh", "rm -rf /\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "repoint"]);
        assert_eq!(
            judge(&root, &base, "repoint"),
            touches(
                &[
                    "infra/forge/reap.sh",
                    "infra/forge/reap2.sh",
                    "infra/ops/verbs/reap.json",
                ],
                &["infra/forge/reap.sh", "infra/ops/verbs/reap.json"],
            ),
            "the old script is named only by the base"
        );

        branch("gone");
        g(&[
            "rm",
            "-q",
            "infra/ops/verbs/reap.json",
            "infra/forge/reap.sh",
        ]);
        g(&["commit", "-qm", "gone"]);
        assert_eq!(
            judge(&root, &base, "gone"),
            touches(
                &["infra/forge/reap.sh", "infra/ops/verbs/reap.json"],
                &["infra/forge/reap.sh", "infra/ops/verbs/reap.json"],
            )
        );

        branch("moved");
        g(&["mv", "infra/forge/reap.sh", "infra/forge/reaped.sh"]);
        g(&["commit", "-qm", "moved"]);
        assert_eq!(
            judge(&root, &base, "moved"),
            touches(&["infra/forge/reap.sh"], &["infra/forge/reap.sh"]),
            "a rename is read as its old path deleted"
        );

        branch("quoted");
        w("infra/forge/é.sh", "rm\n");
        g(&["commit", "-qam", "quoted"]);
        assert_eq!(
            judge(&root, &base, "quoted"),
            touches(&["infra/forge/é.sh"], &[])
        );

        branch("seed");
        w(
            "examples/brewery/seeds/classes.json",
            "[{\"code\":\"x\"}]\n",
        );
        w("examples/brewery/seeds/menu.json", "[1]\n");
        g(&["commit", "-qam", "seed"]);
        assert_eq!(
            judge(&root, &base, "seed"),
            touches(&["examples/brewery/seeds/classes.json"], &[]),
            "a seed a MUTATING verb reads as its delete set holds; one nothing names does not"
        );

        branch("protocol");
        w(
            "infra/platform/workflows/ops-request.toml",
            "[[workflow]]\n# v2\n",
        );
        g(&["commit", "-qam", "protocol"]);
        assert_eq!(
            judge(&root, &base, "protocol"),
            touches(&["infra/platform/workflows/ops-request.toml"], &[])
        );

        assert!(matches!(judge(&root, "", "touch"), Judgement::Unread(_)));
        assert!(matches!(
            judge(&root, "no-such-rev", "touch"),
            Judgement::Unread(_)
        ));
    }

    /// THE CLI THROUGH THE GIT READER (review of 5cf58144, findings 1 and
    /// 2): `side_at` lists the CLI sources, so a car changing the module
    /// a verb runs holds; and main.rs holds when its hunks route to a
    /// reached module — a changed line naming it, or a line added beside
    /// one — while an edit to an arm nothing reaches stays clear.
    #[test]
    fn judge_reads_the_cli_a_verb_runs_and_the_main_arms_that_route_to_it() {
        let (root, g, w) = repo("mutating-verb-judge-cli");
        g(&["init", "-q", "-b", "main"]);
        w(
            "infra/ops/verbs/run-car-probe.json",
            r#"{"about":"runs a car's probe","hosts":["forge"],"argv":["boss","prove","{1}","--from-car","--unattended"]}"#,
        );
        w(&cli("prove.rs"), "pub async fn run_unattended() {}\n");
        w(&cli("job.rs"), "pub fn run() {}\n");
        // The mod lines sit apart from the arms, as they do in the real
        // main.rs: `mod prove` in a hunk's context names prove.
        let main = "mod job;\nmod prove;\n// 1\n// 2\n// 3\n// 4\nfn main() {\n    match cmd {\n        \
                    Commands::Job => job::run(),\n        // a\n        // b\n        // c\n        \
                    // d\n        // e\n        Commands::Prove { car, unattended } => {\n            \
                    if unattended {\n                return prove::run_unattended(&car);\n            \
                    }\n        }\n    }\n}\n";
        w(&cli("main.rs"), main);
        g(&["add", "."]);
        g(&["commit", "-qm", "base"]);
        let base = g(&["rev-parse", "HEAD"]);
        let branch = |name: &str| g(&["checkout", "-q", "-b", name, &base]);

        branch("module");
        w(
            &cli("prove.rs"),
            "pub async fn run_unattended() { evil() }\n",
        );
        g(&["commit", "-qam", "module"]);
        assert_eq!(
            judge(&root, &base, "module"),
            touches(&[&cli("prove.rs")], &[]),
            "side_at must list the CLI sources for this to hold"
        );

        branch("rerouted");
        w(&cli("evil.rs"), "pub async fn run() {}\n");
        w(
            &cli("main.rs"),
            &main.replace("prove::run_unattended(&car)", "evil::run()"),
        );
        g(&["add", "."]);
        g(&["commit", "-qm", "rerouted"]);
        assert_eq!(
            judge(&root, &base, "rerouted"),
            touches(&[&cli("main.rs")], &[]),
            "the removed line names prove"
        );

        branch("beside");
        w(
            &cli("main.rs"),
            &main.replace(
                "            if unattended {",
                "            if car.is_empty() { return evil::run(); }\n            if unattended {",
            ),
        );
        g(&["commit", "-qam", "beside"]);
        assert_eq!(
            judge(&root, &base, "beside"),
            touches(&[&cli("main.rs")], &[]),
            "the added line names nothing reached; its context does"
        );

        // The re-review's D1: the whole prove source swapped under its
        // own name, no arm and no call site touched.
        branch("pathswap");
        w(&cli("evil.rs"), "pub async fn run_unattended() {}\n");
        w(
            &cli("main.rs"),
            &main.replace("mod prove;", "#[path = \"evil.rs\"] mod prove;"),
        );
        g(&["add", "."]);
        g(&["commit", "-qm", "pathswap"]);
        assert_eq!(
            judge(&root, &base, "pathswap"),
            touches(&[&cli("main.rs")], &[]),
            "a #[path on a reached module's declaration holds"
        );

        branch("far");
        w(
            &cli("main.rs"),
            &main.replace("job::run()", "job::run_all()"),
        );
        g(&["commit", "-qam", "far"]);
        assert_eq!(
            judge(&root, &base, "far"),
            Judgement::Clear,
            "an arm nothing a verb runs reaches, far from one that is"
        );

        let reached: BTreeSet<String> = ["car_retire".to_string()].into();
        assert!(main_routes_to(
            "+ Commands::CarRetire { c } => x(c),",
            &reached
        ));
        assert!(!main_routes_to("+ Commands::CarRetired => x(),", &reached));
        assert!(!main_routes_to("@@ -1 +1 @@ fn car_retire::x", &reached));
        assert!(main_routes_to(
            "-mod car_retire;\n+pub mod car_retire;",
            &reached
        ));
        assert!(!main_routes_to("+mod car_retired;", &reached));
        assert!(
            main_routes_to("+#[path = \"x.rs\"]\n mod job;", &reached),
            "a #[path holds whatever module it names"
        );
    }

    /// A tree with no verb on either side — the registry moved — is
    /// unread through the git reader too, not clear.
    #[test]
    fn judge_reads_a_tree_with_no_registry_as_unread() {
        let (root, g, w) = repo("mutating-verb-judge-moved");
        g(&["init", "-q", "-b", "main"]);
        w("infra/forge/x.sh", "rm\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "base"]);
        let base = g(&["rev-parse", "HEAD"]);
        w("infra/forge/x.sh", "rm -rf\n");
        g(&["commit", "-qam", "edit"]);
        let judged = judge(&root, &base, "HEAD");
        assert!(
            matches!(&judged, Judgement::Unread(why) if why.contains("registry moved")),
            "{judged:?}"
        );
    }
}
