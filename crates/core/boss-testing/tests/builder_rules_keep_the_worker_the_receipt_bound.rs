//! The builder rules must say, once, how a run keeps the worker its
//! receipt bound: when the receipt is taken, how a repair adopts a car
//! branch another worktree holds, that no commit is ever rewritten after
//! the receipt, that only an exit 0 lets a push go, and how the pre-flight
//! outlives the harness's foreground bound.
//!
//! MEASURED across six builder runs on 2026-11-18 (backlog 47f42c1a), and
//! before that in the items it closes (69af68dd, e35404e4, 97a85957 and
//! others). Every one is a builder rediscovering a rule this document did
//! not carry: `--started` taken on the harness's worktree branch, or
//! before a repair's checkout, admitted no car or bound the worker to
//! origin/main; `checkout -b <car branch>` failed because a dead builder's
//! worktree still held the name; a `commit --amend` left every later
//! exec-auto refusing; run 5aa3c1e1 pushed f2b0b6a28 after exec-auto
//! refused, so an untested commit reached the forge; and a pre-flight of
//! ~15 minutes in the worker outran a 600 s foreground call while rule 9
//! forbade the background. The text below is what this pin holds the
//! rules to; the dispatch's REPAIR section carries the same checkout
//! command, pinned beside `repair_section` in boss-cli.

use boss_testing::repo_root;

const RULES: &str = "infra/platform/documents/builder-rules.md";

fn rules() -> String {
    std::fs::read_to_string(repo_root().join(RULES)).expect("the builder rules are readable")
}

/// The text of rule `n`: from its number at the start of a line to the
/// next rule's.
fn rule(text: &str, n: u32) -> String {
    let start = text
        .find(&format!("\n{n}. "))
        .unwrap_or_else(|| panic!("{RULES} has no rule {n}"))
        + 1;
    let end = text[start..]
        .find(&format!("\n{}. ", n + 1))
        .map_or(text.len(), |e| e + start);
    text[start..end].to_string()
}

/// The receipt is taken on the car's branch, never before it: the preamble
/// says so, and rule 1 says it for both a fresh car and a repair.
#[test]
fn the_receipt_runs_after_the_car_branch_is_checked_out() {
    let text = rules();
    let preamble = &text[..text.find("\n0. ").expect("rule 0")];
    for want in ["--started", "rule 1"] {
        assert!(
            preamble.contains(want),
            "the opening of {RULES} must place the receipt (`{want}`) after rule \
             1's checkout (backlog f1661ab6, e35404e4): {preamble}"
        );
    }
    let one = rule(&text, 1);
    assert!(
        one.contains("--started"),
        "rule 1 of {RULES} must say the receipt runs after its checkout: {one}"
    );
}

/// A repair adopts the car through a local branch tracking the forge
/// branch and pushes HEAD to the car branch, as the REPAIR section prints.
#[test]
fn a_repair_adopts_the_car_through_a_tracking_branch() {
    let one = rule(&rules(), 1);
    for want in [
        "`git -C <your worktree> checkout -b repair/<car8>-<run8> --track origin/<car branch>`",
        "`git -C <your worktree> push origin HEAD:<car branch>`",
        "fast-forward",
    ] {
        assert!(
            one.contains(want),
            "rule 1 of {RULES} must carry the repair's adoption, `{want}` \
             (backlog 69af68dd, 49a64236): {one}"
        );
    }
}

/// After the receipt every commit descends from the worker's head: no
/// amend, reset, squash or side branch, and the recovery is named.
#[test]
fn no_commit_is_rewritten_after_the_receipt() {
    let five = rule(&rules(), 5);
    for want in ["--amend", "reset", "squash", "side branch", "merge -s ours"] {
        assert!(
            five.contains(want),
            "rule 5 of {RULES} must say `{want}` — after the receipt the worker \
             follows only descendants (backlog 97a85957, 7c6fda9c, 39172573, \
             b58cace6): {five}"
        );
    }
}

/// Rule 5's recovery, run against git exactly as written, must put the
/// worker-bound head back into HEAD's ancestry — the condition
/// `infra/dev/dev-build.py` admits on (`merge-base --is-ancestor synced
/// head`) — AND keep the corrected tree. Review 6a997dec (2026-11-20)
/// measured the first wording, `merge -s ours <the rewritten sha>`: right
/// after an amend HEAD already IS that sha, so the merge is "Already up to
/// date" and the bound head stays outside the ancestry; run from the old
/// head instead it keeps the old tree and discards the correction. A
/// presence pin accepted both, so this one runs the command.
#[test]
fn the_rewrite_recovery_restores_the_bound_head_and_keeps_the_correction() {
    let five = rule(&rules(), 5);
    let command = five
        .split('`')
        .find(|span| span.contains("merge -s ours"))
        .unwrap_or_else(|| panic!("rule 5 of {RULES} names no `merge -s ours` recovery: {five}"));

    let repo = boss_testing::scratch_dir("rewrite-recovery");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "builder")
            .env("GIT_AUTHOR_EMAIL", "builder@example.invalid")
            .env("GIT_COMMITTER_NAME", "builder")
            .env("GIT_COMMITTER_EMAIL", "builder@example.invalid")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .output()
            .expect("git runs");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )
    };
    let write = |body: &str| std::fs::write(repo.join("work.txt"), body).expect("fixture file");

    assert_eq!(git(&["init", "-q"]).0, Some(0));
    write("base\n");
    git(&["add", "work.txt"]);
    assert_eq!(git(&["commit", "-qm", "base"]).0, Some(0));
    // The commit the receipt's worker synced to.
    write("first try\n");
    assert_eq!(git(&["commit", "-qam", "work"]).0, Some(0));
    let bound = git(&["rev-parse", "HEAD"]).1;
    // The rewrite rule 5 forbids: an amend carrying the correction.
    write("corrected\n");
    assert_eq!(git(&["commit", "-q", "--amend", "-am", "work"]).0, Some(0));
    let rewritten = git(&["rev-parse", "HEAD"]).1;
    let corrected_tree = git(&["rev-parse", "HEAD^{tree}"]).1;
    assert_eq!(
        git(&["merge-base", "--is-ancestor", &bound, "HEAD"]).0,
        Some(1),
        "fixture: after the amend the bound head is outside HEAD's ancestry"
    );

    // Fill the command's placeholders from what each one names, and run it
    // from the rewritten head, where a builder stands when the refusal comes.
    let filled: String = command
        .replace("<your worktree>", &repo.display().to_string())
        .split('<')
        .enumerate()
        .map(|(i, part)| match (i, part.split_once('>')) {
            (0, _) => part.to_string(),
            (_, Some((name, rest))) if name.contains("worker") => format!("{bound}{rest}"),
            (_, Some((name, rest))) if name.contains("rewritten") => format!("{rewritten}{rest}"),
            _ => panic!("rule 5's recovery `{command}` has a placeholder this pin cannot fill"),
        })
        .collect();
    let argv: Vec<&str> = filled.split_whitespace().collect();
    assert_eq!(argv.first(), Some(&"git"), "{command}");
    let ran = std::process::Command::new("git")
        .args(&argv[1..])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "builder")
        .env("GIT_AUTHOR_EMAIL", "builder@example.invalid")
        .env("GIT_COMMITTER_NAME", "builder")
        .env("GIT_COMMITTER_EMAIL", "builder@example.invalid")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .output()
        .expect("git runs");
    assert!(
        ran.status.success(),
        "rule 5's recovery `{command}` failed: {}",
        String::from_utf8_lossy(&ran.stderr)
    );

    assert_eq!(
        git(&["merge-base", "--is-ancestor", &bound, "HEAD"]).0,
        Some(0),
        "rule 5's recovery `{command}` must make the worker-bound head an ancestor \
         of HEAD, or dev-build exec-auto keeps refusing (review 6a997dec)"
    );
    assert_eq!(
        git(&["rev-parse", "HEAD^{tree}"]).1,
        corrected_tree,
        "rule 5's recovery `{command}` must keep the corrected tree, not the bound one"
    );
}

/// Rule 6 pushes only on the pre-flight's exit 0; a refusal is a stop.
#[test]
fn the_push_waits_for_an_exit_zero() {
    let six = rule(&rules(), 6);
    for want in ["exit 0", "75", "stop"] {
        assert!(
            six.contains(want),
            "rule 6 of {RULES} must make the push wait for the pre-flight's exit 0 \
             and name a refusal as a stop (`{want}`; run 5aa3c1e1 pushed an \
             untested commit): {six}"
        );
    }
}

/// The pre-flight's verdict outlives the foreground bound: the script
/// writes its own exit status to a file, it runs in the harness's own
/// background mode, which rule 9 allows for it alone, and a missing
/// status is no verdict.
#[test]
fn the_preflight_says_how_it_outlives_the_foreground_bound() {
    let text = rules();
    let sixteen = rule(&text, 16);
    for want in ["preflight.rc", "600", "background", "no verdict"] {
        assert!(
            sixteen.contains(want),
            "rule 16 of {RULES} must say how the pre-flight outlives the harness's \
             foreground bound (`{want}`; backlog 54c772b6): {sixteen}"
        );
    }
    let nine = rule(&text, 9);
    assert!(
        nine.contains("rule 16"),
        "rule 9 of {RULES} forbids background tasks and must name rule 16's \
         pre-flight as the one it allows: {nine}"
    );
}
