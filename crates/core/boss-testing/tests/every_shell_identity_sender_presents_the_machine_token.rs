//! Every shell script under infra/ that signs a request to the system of
//! record (`x-boss-user`) also PRESENTS the estate machine token, through
//! the one reader — or is listed here with the reason it does not
//! (design 6805c764; backlog 2710c8fc).
//!
//! WHY. Every service's machine gate records, and under `enforce` will
//! refuse, a request without the token. The Rust half has had this pin
//! since car 2 (`every_x_boss_user_sender_stamps_the_machine_token.rs`);
//! the shell half had only `every_shell_sender_reads_the_machine_token_
//! from_its_mount.rs`, which holds HOW a script takes the token and says
//! nothing about a script that takes none. Measured 2026-10-06: seven
//! send sites in six host libraries signed a request and never called
//! the reader, and they were found by reading three days of
//! `machine_gate.would_refuse` facts, not by a check.
//!
//! WHAT IT READS. The sender list is DERIVED, never written down: every
//! shell file under infra/ (a `.sh` name, or a `#!` line naming sh or
//! bash) with a live — non-comment — line naming `x-boss-user`. Such a
//! file passes when a live line calls the reader
//! (`machine_token_header <VAR> …`), or hands curl a header file its
//! caller's reader call made:
//!
//!     ${…MT_HDR:+-H "$…MT_HDR"}        (or …TOKEN_HDR…, the pod door's)
//!
//! That variable can only be filled by `machine_token_header` — the
//! mount pin refuses a header written by hand. A file that signs and
//! does neither is refused by name unless NOT_YET or BY_DESIGN lists it.
//! It is a NAME scan: a file that calls the reader and then forgets to
//! hand curl the file passes here, and is caught where the behaviour is
//! run (`host_senders_present_the_machine_token.rs` for the host libs).
//!
//! THE TWO LISTS ARE NOT THE SAME KIND OF ENTRY. BY_DESIGN names a
//! sender that must not hold the token, or for which that is an open,
//! recorded decision. NOT_YET names a sender of the same shape as the
//! ones this car stamped, left for a later car — a debt, with the reason
//! it was not paid here. A listed file that has since been stamped is
//! not an error (two cars may stamp and list the same file; a pin that
//! went red on the assembled tree for that would punish the fix), so a
//! stale entry is reported by `stale_entries_are_named`, which prints and
//! passes.
//!
//! OUT OF SCOPE: a script that sends NO identity at all. The workflow
//! publish's live read was one (infra/gcp/publish-workflow.sh, stamped
//! in this car and pinned by name below); only the gate's own record
//! finds the next.
//!
//! tree-wide pin — it scans every file under infra/, which no
//! changed-file map attributes to this crate, so every scoped gate runs
//! it whatever its scope (`tree_wide_pins` in infra/gate.sh).

use boss_testing::repo_root;
use regex::Regex;
use std::path::{Path, PathBuf};

/// Senders that hold no token on purpose, or by a decision on the record.
const BY_DESIGN: &[(&str, &str)] = &[
    (
        "infra/gate-runner/run.sh",
        "in-cluster non-holder: the gate runs a car's own code, so whether it may hold the token is the decision owed before enforce (2710c8fc before_enforce)",
    ),
    (
        "infra/cluster/dev-scratch-reclaim.sh",
        "the dev pod's sidecar: the pod's token is moving behind an uncredentialed worker (design c395e62c), decided with that isolation",
    ),
    (
        "infra/forge/probe-bin/boss-sor-read",
        "the recorded probe's reader runs car-supplied shell on the forge and must not hold the estate token; its own reader door is backlog d26515c5",
    ),
    (
        "infra/forge/recovery-kit-read.sh",
        "installed root-owned outside the checkout and cannot source the reader (its own header says so)",
    ),
    (
        "infra/wait-out-a-converge.sh",
        "run by the in-cluster playground crawl, a non-holder owed the same decision as the gate runner",
    ),
    (
        "infra/postgres/reset-to-baseline.sh",
        "an operator's tool against a local stack, not a caller of the estate",
    ),
    (
        "infra/postgres/validate-brewery-sim.sh",
        "an operator's tool against a local stack, not a caller of the estate",
    ),
    (
        "infra/record-agent-runs.sh",
        "an operator's hand tool; the session's own door (boss-api) is the stamped path",
    ),
];

/// Lints: a lint is a question about a tree and holds no token by rule
/// (infra/lint/a-lint-sources-a-header-lib-only-hermetic.sh; the runners
/// hand every lint an empty token directory). The live readers among
/// them are that lint's one exception, judged there.
const BY_DESIGN_DIRS: &[&str] = &["infra/lint/"];

/// Host senders of the shape stamped on 2026-10-06, not stamped by that
/// car. Each is owed a reader call; the list is a debt, not a licence.
const NOT_YET: &[(&str, &str)] = &[
    (
        "infra/codebase-observe/observe-codebase.sh",
        "a host timer's POST; outside the sites measured for the stamping car",
    ),
    (
        "infra/forge/discover-admission-source.sh",
        "a read-only ops verb under its own approved design (25bdb2cc), awaiting independent source review",
    ),
    (
        "infra/forge/probe-admission-source-access.sh",
        "a read-only ops verb under its own approved design (f927de0c), awaiting independent source review",
    ),
    (
        "infra/gcp/retire-cloudflared.sh",
        "a one-shot retire verb on boss-gcp",
    ),
    (
        "infra/ops/retire-ops-runner.sh",
        "a one-shot retire verb, run from inside the runner pass",
    ),
    (
        "infra/install-smoke/nightly.sh",
        "the nightly install smoke's two packet writes",
    ),
];

/// Files this pin holds by NAME as well, because their request signs
/// nothing and the derivation above cannot see them.
const UNSIGNED_BUT_STAMPED: &[&str] = &["infra/gcp/publish-workflow.sh"];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.filter_map(Result::ok) {
        let p = entry.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn is_shell(path: &Path, text: &str) -> bool {
    if path.extension().is_some_and(|x| x == "sh") {
        return true;
    }
    path.extension().is_none()
        && text.lines().next().is_some_and(|l| {
            l.starts_with("#!") && (l.ends_with("sh") || l.contains("bash") || l.contains("/sh "))
        })
}

fn live(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter(|l| !l.trim_start().starts_with('#'))
}

fn signs(text: &str) -> bool {
    live(text).any(|l| l.contains("x-boss-user"))
}

/// A live line that takes the token from the one reader — a
/// `machine_token_header <VAR> …` call — or that hands curl a header file
/// a caller's reader call made: `${X:+-H "$X"}` with one variable, whose
/// name ends `MT_HDR` or `TOKEN_HDR` (infra/forge/publish-pr-state.sh is
/// handed its caller's). The four deposit and train-key scripts pass on
/// the call: they put the file into an argv array rather than the `:+`
/// form.
fn presents(text: &str) -> bool {
    let call = Regex::new(r"(^|[\s;&|({!])machine_token_header\s+[A-Za-z_]+").unwrap();
    let hand = Regex::new(r#"\$\{([A-Za-z_]*(?:MT|TOKEN)_HDR):\+-H "\$([A-Za-z_]+)"\}"#).unwrap();
    live(text).any(|l| call.is_match(l) || hand.captures_iter(l).any(|c| c[1] == c[2]))
}

/// (rel path, signs, presents) for every shell file under `root`/infra.
fn senders(root: &Path) -> Vec<(String, bool, bool)> {
    let mut files = Vec::new();
    walk(&root.join("infra"), &mut files);
    files.sort();
    files
        .into_iter()
        .filter_map(|f| {
            let text = std::fs::read_to_string(&f).ok()?;
            if !is_shell(&f, &text) {
                return None;
            }
            let rel = f.strip_prefix(root).ok()?.to_string_lossy().into_owned();
            Some((rel, signs(&text), presents(&text)))
        })
        .collect()
}

fn listed(rel: &str) -> bool {
    BY_DESIGN.iter().chain(NOT_YET).any(|(p, _)| *p == rel)
        || BY_DESIGN_DIRS.iter().any(|d| rel.starts_with(d))
}

#[test]
fn every_shell_identity_sender_presents_the_machine_token() {
    let all = senders(&repo_root());
    assert!(
        all.iter().filter(|(_, s, p)| *s && *p).count() >= 20,
        "the scan found fewer than twenty stamped senders — it has stopped seeing the tree, and a scanner that sees nothing passes everything"
    );
    let missing: Vec<&str> = all
        .iter()
        .filter(|(rel, signs, presents)| *signs && !*presents && !listed(rel))
        .map(|(rel, _, _)| rel.as_str())
        .collect();
    assert!(
        missing.is_empty(),
        "these scripts sign a request to the system of record (x-boss-user) and never present \
         the estate machine token, so every machine gate records them as a caller it would \
         refuse. Call the one reader before the request and hand curl its file — \
         `machine_token_header MT_HDR \"$BASE\" || MT_HDR=\"\"` then `${{MT_HDR:+-H \"$MT_HDR\"}}` \
         (infra/lib/secret-header.sh; the token is presented, never required) — or list the \
         file in BY_DESIGN / NOT_YET here with its reason:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn the_unsigned_live_read_stays_stamped() {
    let all = senders(&repo_root());
    for rel in UNSIGNED_BUT_STAMPED {
        let row = all.iter().find(|(r, _, _)| r == rel);
        assert!(
            row.is_some_and(|(_, _, presents)| *presents),
            "{rel} sends a request with no identity, which the derivation cannot see; it is held \
             by name, and it no longer hands curl the reader's header file"
        );
    }
}

/// A listed file that is gone, no longer signs, or is now stamped. Named
/// so the list can be trimmed; not a failure (see the module doc).
#[test]
fn stale_entries_are_named() {
    let all = senders(&repo_root());
    for (rel, why) in BY_DESIGN.iter().chain(NOT_YET) {
        match all.iter().find(|(r, _, _)| r == rel) {
            None => println!(
                "stale: {rel} is listed and is no longer a shell file under infra/ ({why})"
            ),
            Some((_, false, _)) => println!("stale: {rel} is listed and signs nothing now ({why})"),
            Some((_, true, true)) => println!("stale: {rel} is listed and is stamped now ({why})"),
            Some((_, true, false)) => {}
        }
    }
}

#[test]
fn the_matchers_see_a_sender_and_a_presentation() {
    assert!(signs("curl -H \"x-boss-user: $U\" \"$BASE/api/jobs\"\n"));
    assert!(!signs(
        "# curl -H 'x-boss-user: …' is what this does NOT do\n"
    ));
    assert!(presents("curl ${MT_HDR:+-H \"$MT_HDR\"} \"$u\"\n"));
    assert!(presents(
        "  ${ALERT_PEOPLE_MT_HDR:+-H \"$ALERT_PEOPLE_MT_HDR\"} \\\n"
    ));
    assert!(presents("curl ${TOKEN_HDR:+-H \"$TOKEN_HDR\"} \"$u\"\n"));
    assert!(!presents("# ${MT_HDR:+-H \"$MT_HDR\"}\n"));
    assert!(!presents("curl ${AUTH_HDR:+-H \"$AUTH_HDR\"} \"$u\"\n"));
    assert!(!presents("curl ${MT_HDR:+-H \"$OTHER\"} \"$u\"\n"));
    assert!(presents(
        "machine_token_header MT_HDR \"$BASE\" || MT_HDR=\"\"\n"
    ));
    assert!(presents(
        "    if ! machine_token_header MT_HDR \"$BASE\"; then\n"
    ));
    assert!(!presents("# machine_token_header MT_HDR \"$BASE\"\n"));
    assert!(!presents("declare -F machine_token_header >/dev/null\n"));
}

/// The list holds no sender twice and none that is also a directory rule.
#[test]
fn each_file_is_listed_once() {
    let mut seen: Vec<&str> = Vec::new();
    for (rel, why) in BY_DESIGN.iter().chain(NOT_YET) {
        assert!(!seen.contains(rel), "{rel} is listed twice");
        assert!(!why.is_empty(), "{rel} is listed with no reason");
        assert!(
            !BY_DESIGN_DIRS.iter().any(|d| rel.starts_with(d)),
            "{rel} is under a listed directory already"
        );
        seen.push(rel);
    }
}
