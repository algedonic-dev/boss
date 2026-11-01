//! `infra/dev/hooks/shared-checkout-guard.py` and `infra/dev/restore-shared`
//! — a worktree-isolated agent cannot write the shared checkout, and can
//! put back a file it already dirtied there (backlog 10d1567b).
//!
//! THE INCIDENT, 2026-10-10 ~08:3xZ. Builder run 63c60863 ran `cd <its
//! worktree> && sed -i …`; the harness dropped the cd (measured, backlog
//! 916fe290: every Bash call of an isolated agent starts in the
//! dispatching session's checkout, `/work/boss`), so the sed edited two
//! source files THERE. The builder restored one; Claude Code's own
//! worktree-isolation guard refused its `git -C /work/boss checkout --`
//! repair of the other, because that guard reads only git. So the
//! damage went through and the repair did not, and the coordinator
//! restored the file by hand at ~09:12Z.
//!
//! THE HOOK sees what the harness is about to run — measured on Claude
//! Code 2.1.296 with scratch projects: an isolated subagent's PreToolUse
//! payload carries `agent_id` and `cwd` = `.claude/worktrees/agent-<agent_id>`.
//! Agent-definition frontmatter `hooks:` did NOT fire, so the hook is
//! registered in `.claude/settings.json`, and scoped by the payload rather
//! than by the file it lives in. Where a call then RUNS differs by harness
//! path (916fe290 on the production dispatch: /work/boss; review 0dc61dd6
//! under `claude -p`: the worktree, with the leading cd stripped before
//! the hook), so the hook judges each path as written from the reported
//! cwd and passes what it cannot resolve.
//!
//! THE DOOR is the repair the isolation guard refuses: a tracked file in
//! the shared checkout, changed within the hour, put back to HEAD with
//! the discarded change saved first.

use boss_testing::{create_dir, feed_stdin, git_config_isolated, repo_root, scratch_dir};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

const HOOK: &str = "infra/dev/hooks/shared-checkout-guard.py";
const DOOR: &str = "infra/dev/restore-shared";

/// The shared checkout and an agent's worktree under it — paths only;
/// the hook decides from text, it never touches them.
const SHARED: &str = "/work/boss";
const WT: &str = "/work/boss/.claude/worktrees/agent-a1b2c3";

fn payload(agent: bool, cwd: &str, tool: &str, input: serde_json::Value) -> String {
    let mut p = serde_json::json!({
        "session_id": "s-1",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": input,
    });
    if agent {
        p["agent_id"] = "a1b2c3".into();
        p["agent_type"] = "effort-high".into();
    }
    p.to_string()
}

fn bash(command: &str) -> String {
    payload(true, WT, "Bash", serde_json::json!({ "command": command }))
}

/// Where the hook keeps a call's before-picture: this process's own
/// scratch, never the pod's shared /tmp.
fn hook_tmp() -> &'static Path {
    static TMP: OnceLock<PathBuf> = OnceLock::new();
    TMP.get_or_init(|| scratch_dir("shared-checkout-guard-tmp"))
}

fn run_hook(stdin: &str) -> (i32, String) {
    run_hook_in(stdin, hook_tmp(), &[])
}

fn run_hook_in(stdin: &str, tmp: &Path, env: &[(&str, &Path)]) -> (i32, String) {
    let mut child = Command::new(repo_root().join(HOOK))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", tmp)
        .envs(env.iter().copied())
        .env("CLAUDE_PROJECT_DIR", SHARED)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("run {HOOK}: {e}"));
    feed_stdin(&mut child, stdin.as_bytes());
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[track_caller]
fn refused(command: &str, names: &str) {
    let (code, err) = run_hook(&bash(command));
    assert_eq!(code, 2, "must refuse {command:?}; stderr: {err}");
    assert!(err.contains(names), "the refusal names {names:?}: {err}");
}

#[track_caller]
fn passed(command: &str) {
    let (code, err) = run_hook(&bash(command));
    assert_eq!(code, 0, "must pass {command:?}; stderr: {err}");
}

/// A path is judged AS WRITTEN, from BOTH places the call may run. Where
/// a bare relative path lands depends on the harness path, and the two
/// measured disagree: on the production dispatch 916fe290 saw an isolated
/// agent's Bash start in /work/boss, while review 0dc61dd6 (`claude -p`,
/// 2.1.296) saw it start in the worktree and land a relative `sed -i`
/// there. Until backlog 88553fea a bare relative write PASSED on that
/// uncertainty, and on 2026-10-20 run 472b2b58's two mutation seds dirtied
/// /work/boss through exactly that hole — re-measured the same day by
/// builder run 7934069e, whose payload named its worktree while `pwd -P`
/// printed /work/boss. So a write that lands in the shared checkout from
/// EITHER base is refused; the way out (an absolute path, or a script that
/// cds first) costs one retry, and the miss cost the pod its doors.
#[test]
fn a_bare_relative_write_is_judged_from_both_places_it_may_run() {
    refused(
        "sed -i 's/fn names_file/pub(crate) fn names_file/' crates/x/judged_red.rs",
        "/work/boss/crates/x/judged_red.rs",
    );
    refused("perl -pi -e 's/a/b/' crates/x.rs", "/work/boss/crates/x.rs");
    refused("echo hi > notes.txt", "/work/boss/notes.txt");
    refused("printf x | tee crates/y.rs", "/work/boss/crates/y.rs");
    refused("cargo fmt -p boss-cli", "/work/boss");
    // A relative cd moves both bases, and lands in the shared checkout
    // from the one the production path starts in.
    refused("cd crates && sed -i s/a/b/ x.rs", "/work/boss/crates/x.rs");
    // Once an ABSOLUTE cd has taken effect there is one base, and it is
    // known: the same writes from inside the worktree pass.
    passed(&format!("cd {WT}; sed -i s/a/b/ crates/x.rs"));
    passed(&format!("cd {WT}; echo hi > notes.txt"));
    refused(
        "sed -i s/a/b/ ../../../crates/orchestrators/boss-cli/src/train/judged_red.rs",
        "/work/boss/crates/orchestrators/boss-cli/src/train/judged_red.rs",
    );
    refused("echo hi > ../../../notes.txt", "/work/boss/notes.txt");
    refused(
        "cp /tmp/a.rs ../../../crates/a.rs",
        "/work/boss/crates/a.rs",
    );
}

/// GIT THAT MOVES A TREE names its tree (backlog 88553fea). Measured from
/// the /work/boss reflog on 2026-10-20: reviewer runs 2233667d and
/// c1a5a342 ran `git checkout --detach <sha>` there, and run 2452d6ff
/// switched it to its own branch `worktree-agent-adc4e55861d5bfd66` and
/// left it there thirteen minutes — every pod door, being a symlink into
/// /work/boss/infra/dev, ran another agent's branch meanwhile. Each was a
/// bare git whose leading `cd <worktree> &&` the harness had dropped.
/// The harness's own guard refuses `git -C /work/boss`; it does not see a
/// bare git that RUNS there. So a git that moves HEAD, the index or the
/// work tree is refused unless its tree is named as the caller's own
/// worktree, and the refusal names that worktree and `git -C`.
#[test]
fn a_git_that_moves_a_tree_must_name_the_worktree() {
    for cmd in [
        "git checkout --detach 8a9fdecb",
        "git switch worktree-agent-adc4e55861d5bfd66",
        "git checkout -b fix/x origin/main",
        "git reset --hard origin/main",
        "git restore crates/x.rs",
        "git stash",
        "git stash push -u -m tag",
        "git commit -F /tmp/s/commit-msg.txt",
        "git -c core.pager=cat checkout main",
        "env GIT_TRACE=0 git checkout main",
        "git -C /work/boss checkout main",
        "cd /work/boss; git switch main",
        "git -C ../../.. reset --hard",
        "git -C /work/boss/.claude/worktrees/agent-ffff00 checkout main",
        // A tree named by --git-dir / --work-tree / GIT_DIR / GIT_WORK_TREE
        // is judged like -C (review e5607585, finding 4: each passed).
        "git --git-dir=/work/boss/.git --work-tree=/work/boss commit -am x",
        "git --git-dir /work/boss/.git commit -am x",
        "git --work-tree /work/boss checkout -- .",
        "GIT_DIR=/work/boss/.git git commit -am x",
        "GIT_WORK_TREE=/work/boss git checkout -- .",
        &format!("GIT_DIR={WT}/.git GIT_WORK_TREE=/work/boss git checkout -- ."),
    ] {
        let (code, err) = run_hook(&bash(cmd));
        assert_eq!(code, 2, "must refuse {cmd:?}; stderr: {err}");
        assert!(
            err.contains(&format!("git -C {WT}")),
            "the refusal for {cmd:?} names the caller's worktree and git -C: {err}"
        );
        assert!(
            err.contains("88553fea"),
            "the refusal for {cmd:?} names the packet it comes from: {err}"
        );
    }
    // The tree named, by -C or by a cd that took effect: passes.
    passed(&format!("git -C {WT} checkout -b fix/x origin/main"));
    passed(&format!("git -C {WT} commit -F /tmp/s/commit-msg.txt"));
    passed(&format!("git -C {WT}/crates restore x.rs"));
    passed(&format!("cd {WT}; git stash push -u -m tag"));
    passed(&format!(
        "git -C {WT} --git-dir={WT}/.git --work-tree={WT} commit -F /tmp/s/m"
    ));
    passed(&format!(
        "GIT_DIR={WT}/.git GIT_WORK_TREE={WT} git -C {WT} commit -F /tmp/s/m"
    ));
    // Git that moves no tree passes wherever it runs: reads, a fetch,
    // and the two stash subcommands that only read.
    for cmd in [
        "git status",
        "git log --oneline -3",
        "git diff origin/main...HEAD",
        "git show origin/main:infra/dev/boss-api",
        "git fetch origin",
        "git stash list",
        "git stash show -p",
        "git rev-parse HEAD",
    ] {
        passed(cmd);
    }
}

/// The writers it reads, each refused on an absolute shared-checkout path.
#[test]
fn every_writer_it_reads_is_refused_on_a_shared_path() {
    refused("sed -i.bak -e s/a/b/ /work/boss/x.rs", "/work/boss/x.rs");
    refused("perl -pi -e 's/a/b/' /work/boss/x.rs", "/work/boss/x.rs");
    refused("printf x >> /work/boss/a/b.rs", "/work/boss/a/b.rs");
    refused("printf x >/work/boss/a/b.rs", "/work/boss/a/b.rs");
    refused(
        "git -C /tmp show HEAD:x | tee /work/boss/y.rs",
        "/work/boss/y.rs",
    );
    refused("rm /work/boss/Cargo.lock", "/work/boss/Cargo.lock");
    refused("cd /work/boss; cargo fmt -p boss-cli", "/work/boss");
    refused("cd /work/boss; sed -i s/a/b/ x.rs", "/work/boss/x.rs");
}

/// Only the CALLER'S OWN worktree is exempt (review finding 6): another
/// agent's worktree, and the worktrees directory itself, are the shared
/// checkout as far as this agent is concerned — `mv` of the directory
/// would take out every builder's tree at once.
#[test]
fn another_agents_worktree_is_not_exempt() {
    refused(
        "rm -rf /work/boss/.claude/worktrees/agent-ffff00",
        "/work/boss/.claude/worktrees/agent-ffff00",
    );
    refused(
        "mv /work/boss/.claude/worktrees /tmp/x",
        "/work/boss/.claude/worktrees",
    );
    refused(
        "echo x > /work/boss/.claude/worktrees/agent-ffff00/x",
        "agent-ffff00/x",
    );
    refused("echo x > ../agent-ffff00/x", "agent-ffff00/x");
}

/// A target it cannot resolve is refused by nothing (review finding 5):
/// the guard's own rule is that a guard which refuses because it cannot
/// SEE is worse than the miss it prevents.
#[test]
fn a_target_it_cannot_resolve_passes() {
    passed("echo x > \"$TMPDIR/out.log\"");
    passed("bash /tmp/s.sh > \"$HOME/log\" 2>&1");
    passed("echo x > \"$(mktemp)\"");
    passed("echo x > `mktemp`");
    passed("sed -i s/a/b/ $f");
    passed("tee -a ${LOG:-/tmp/x} < /dev/null");
}

/// AN INTERPRETER'S INLINE PROGRAM THAT WRITES is judged like a writer
/// (backlog ab0702d2). Twice on 2026-10-22 (runs adc74f96 and 2f9b5353) a
/// builder ran `cd <worktree> && python3 -I - <<'PY'` whose program did
/// `p='crates/core/boss-jobs/src/membership_history_pg.rs'`, read it,
/// `.replace`d a block and `open(p,'w').write(...)` it back. The hook saw
/// the command with its cd already stripped — the transcript records
/// `python3 -I - <<'PY'` at the worktree cwd — and passed it, because
/// python3 was no writer it read; bash ran it in /work/boss, and
/// infra/dev/restore-shared repaired both. The program text is not
/// parsed, so the rule is a net: an inline program (`-c`, `-e`, `-`, a
/// heredoc or here-string) that carries a write call is refused when a
/// path it names is relative and no absolute cd has fixed where it runs,
/// or when it names no absolute path at all, or names one in the shared
/// checkout outside the worktree.
#[test]
fn an_inline_program_that_writes_from_the_shared_checkout_is_refused() {
    // The incident's own shape, as the hook saw it.
    refused(
        "python3 -I - <<'PY'\np='crates/core/boss-jobs/src/membership_history_pg.rs'\ns=open(p).read()\ns=s.replace('a','b')\nopen(p,'w').write(s)\nPY",
        "python3",
    );
    refused(
        "python3 -c \"from pathlib import Path; Path('crates/x.rs').write_text('x')\"",
        "crates/x.rs",
    );
    refused(
        "perl -e 'open(my $f, \">\", \"crates/x.rs\"); print $f 1'",
        "perl",
    );
    refused(
        "node -e \"require('fs').writeFileSync('apps/web/x.ts', '')\"",
        "node",
    );
    refused("bun -e \"await Bun.write('apps/web/x.ts', '')\"", "bun");
    // A path the program builds, so nothing absolute is named: not seen,
    // so not known to be safe.
    refused(
        "python3 -c 'import sys; open(sys.argv[1], \"w\").write(\"\")' notes.txt",
        "python3",
    );
    // An absolute path into the shared checkout, from anywhere.
    refused(
        &format!("cd {WT}; python3 -c \"open('/work/boss/crates/x.rs','w').write('')\""),
        "/work/boss/crates/x.rs",
    );
    // What builders run all day passes: reading programs, writes to the
    // scratchpad or the worktree by absolute path, a program after an
    // absolute cd, a module, a script by path.
    passed(
        "boss-api GET /api/jobs?limit=1 | python3 -c 'import json,sys; print(json.load(sys.stdin)[\"total\"])'",
    );
    passed(
        "python3 -I -c 'import json,sys; [print(l[:80]) for l in open(sys.argv[1])]' crates/x.rs",
    );
    passed("python3 -I - <<'PY'\nimport json\nprint(json.load(open('crates/x.json'))['a'])\nPY");
    passed("python3 -c \"import json; json.dump({}, open('/tmp/s/out.json', 'w'))\"");
    passed(&format!(
        "python3 -c \"open('{WT}/crates/x.rs','w').write('x')\""
    ));
    passed(&format!(
        "cd {WT}; python3 -c \"open('crates/x.rs','w').write('x')\""
    ));
    passed("python3 -m json.tool crates/x.json");
    passed("python3 /tmp/s/fix.py crates/x.rs");
    passed("python3 -I /tmp/s/keys.py /tmp/s/tail.json");
}

/// A quoted absolute INPUT does not establish where a separate write lands
/// (review a008012c, ab0702d2). Every write needs a known absolute target
/// while either possible cwd is the shared checkout.
#[test]
fn an_absolute_read_does_not_hide_an_unknown_inline_write() {
    for command in [
        "python3 -I - <<'PY'\nfrom pathlib import Path\nblock=Path('/tmp/block.rs').read_text()\nm='rebuild'\nopen(f'crates/core/boss-jobs/src/{m}.rs','w').write(block)\nPY",
        "python3 -c \"from pathlib import Path; s=Path('/tmp/block.rs').read_text(); [p.write_text(s) for p in Path('crates').rglob('*_pg.rs')]\"",
        "python3 -c \"import os; s=open('/tmp/block.rs').read(); open(os.path.join('crates','core','lib'),'w').write(s)\"",
        "python3 -c \"s=open('/tmp/block.rs').read(); open('Makefile','w').write(s)\"",
        "python3 -c \"s=open('/tmp/block.rs').read(); open('/tmp/out','w').write(s); open('Makefile','w').write(s)\"",
        "node -pe \"require('fs').writeFileSync('apps/web/x.ts','')\"",
        "bun -pe \"await Bun.write('apps/web/x.ts','')\"",
    ] {
        refused(command, "inline program");
    }
    // A harmless read never supplies the evidence for a different call's
    // target, but literal absolute writes and an explicit cwd still work.
    passed("python3 -c \"s=open('Makefile').read(); open('/tmp/out','w').write(s)\"");
    passed("python3 -c \"from pathlib import Path; Path('/tmp/out').write_text('x')\"");
    passed("node -pe \"require('fs').writeFileSync('/tmp/out','')\"");
    passed(&format!(
        "cd {WT}; python3 -c \"open('Makefile','w').write(open('/tmp/block.rs').read())\""
    ));
}

/// The dropped cd is refused by its shape, with the ways that work.
#[test]
fn a_leading_cd_into_the_worktree_is_refused_with_the_ways_that_work() {
    let (code, err) = run_hook(&bash(&format!("cd {WT} && sed -i s/a/b/ x.rs")));
    assert_eq!(code, 2, "{err}");
    for way in ["git -C", "absolute", "script"] {
        assert!(err.contains(way), "the refusal names {way:?}: {err}");
    }
    // Even with nothing after it that writes: the shape itself is the trap.
    refused(&format!("cd {WT} && pwd"), "drops");
}

/// An absolute path into the shared checkout is refused the same way.
#[test]
fn an_absolute_write_into_the_shared_checkout_is_refused() {
    refused(
        "sed -i s/a/b/ /work/boss/crates/x.rs",
        "/work/boss/crates/x.rs",
    );
    refused("echo x > /work/boss/.claude/settings.json", "settings.json");
    refused("tee -a /work/boss/a.txt < /tmp/x", "/work/boss/a.txt");
}

/// What a builder does all day passes: absolute worktree paths, the
/// scratchpad, reads, fd plumbing, a cd that takes effect, heredoc text.
#[test]
fn worktree_and_scratch_writes_and_reads_pass() {
    passed(&format!("sed -i s/a/b/ {WT}/crates/x.rs"));
    passed(&format!("echo x > {WT}/notes.txt"));
    passed("bash /tmp/s/gate.sh > /tmp/s/gate.log 2>&1");
    passed("grep -rn foo crates > /dev/null 2>&1");
    passed("sed -n 1,20p crates/x.rs");
    passed("cat crates/x.rs | head -3");
    passed(&format!("git -C {WT} diff --stat"));
    passed("echo 'a > b.rs' > /tmp/s/note");
    // `cd X;` and a cd into a subdirectory take effect (916fe290's
    // measurement), so a relative path after one is judged from there.
    passed(&format!("cd {WT}; sed -i s/a/b/ x.rs"));
    passed(&format!("cd {WT}/crates && echo x > y.rs"));
    passed(
        "cat > /tmp/s/probe.sh <<'EOF'\necho a > b.rs\nsed -i s/x/y/ z.rs\nEOF\nbash /tmp/s/probe.sh",
    );
}

/// Scope: an isolated agent only — one whose `agent_id` names the
/// worktree its cwd is in (`agent-<agent_id>`, measured: review 0dc61dd6's
/// payload dump has agent_id a42a9dceadb21aa39 at cwd
/// `.claude/worktrees/agent-a42a9dceadb21aa39`). The operator's session
/// (no agent_id), an agent that runs in the shared checkout by design,
/// and a NON-isolated subagent of a session that itself works in a
/// worktree (review finding 3) keep their cwd; a payload the hook cannot
/// read refuses nothing.
#[test]
fn only_an_isolated_agent_is_judged_and_an_unreadable_payload_passes() {
    let cmd = serde_json::json!({ "command": "sed -i s/a/b/ /work/boss/x.rs" });
    let (code, err) = run_hook(&payload(false, WT, "Bash", cmd.clone()));
    assert_eq!(code, 0, "the operator's own session: {err}");
    let (code, err) = run_hook(&payload(true, SHARED, "Bash", cmd.clone()));
    assert_eq!(code, 0, "a non-isolated agent: {err}");
    for cwd in [
        "/work/boss/.claude/worktrees/wt1",
        "/work/boss/.claude/worktrees/agent-ffff00",
    ] {
        let rel = serde_json::json!({ "command": "echo hi > rel-sub.txt" });
        let (code, err) = run_hook(&payload(true, cwd, "Bash", rel));
        assert_eq!(code, 0, "a subagent of a session working in {cwd}: {err}");
    }
    let (code, err) = run_hook(&payload(true, &format!("{WT}/crates"), "Bash", cmd));
    assert_eq!(
        code, 2,
        "the isolated agent, from a subdirectory of its worktree: {err}"
    );
    assert_eq!(run_hook("not json").0, 0);
    assert_eq!(run_hook("").0, 0);
    passed("echo 'unbalanced");
}

/// The Write, Edit and NotebookEdit tools take an absolute path; the
/// same line holds for them.
#[test]
fn the_file_tools_are_held_to_the_same_line() {
    for (tool, key) in [
        ("Write", "file_path"),
        ("Edit", "file_path"),
        ("NotebookEdit", "notebook_path"),
    ] {
        let (code, err) = run_hook(&payload(
            true,
            WT,
            tool,
            serde_json::json!({ key: "/work/boss/crates/x.rs" }),
        ));
        assert_eq!(code, 2, "{tool} into the shared checkout: {err}");
        let (code, err) = run_hook(&payload(
            true,
            WT,
            tool,
            serde_json::json!({ key: format!("{WT}/crates/x.rs") }),
        ));
        assert_eq!(code, 0, "{tool} into the worktree: {err}");
    }
}

/// Registered where it fires: project settings, PreToolUse, a matcher
/// that covers Bash and the file tools, run from $CLAUDE_PROJECT_DIR.
#[test]
fn the_guard_is_registered_for_bash_and_the_file_tools() {
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join(".claude/settings.json")).expect("settings"),
    )
    .expect("settings.json is JSON");
    let groups = settings["hooks"]["PreToolUse"]
        .as_array()
        .expect("PreToolUse hooks");
    let group = groups
        .iter()
        .find(|g| {
            g["hooks"].as_array().is_some_and(|hs| {
                hs.iter().any(|h| {
                    h["command"]
                        .as_str()
                        .is_some_and(|c| c.contains(HOOK) && c.contains("$CLAUDE_PROJECT_DIR"))
                })
            })
        })
        .unwrap_or_else(|| panic!("a PreToolUse group runs {HOOK}: {groups:?}"));
    let matcher = group["matcher"].as_str().unwrap_or("");
    let tools: Vec<&str> = matcher.split('|').collect();
    for tool in ["Bash", "Write", "Edit", "NotebookEdit"] {
        assert!(tools.contains(&tool), "matcher {matcher:?} covers {tool}");
    }
    // And AFTER a Bash call, where it reads what the call actually did to
    // the shared checkout (backlog aebd7439) — on both ends a call can
    // have: PostToolUse, and PostToolUseFailure for a call that exited
    // nonzero, which a script that commits and then fails a check does.
    for event in ["PostToolUse", "PostToolUseFailure"] {
        let after = settings["hooks"][event]
            .as_array()
            .unwrap_or_else(|| panic!("{event} hooks"));
        assert!(
            after.iter().any(|g| {
                g["matcher"]
                    .as_str()
                    .is_some_and(|m| m.split('|').any(|t| t == "Bash"))
                    && g["hooks"].as_array().is_some_and(|hs| {
                        hs.iter().any(|h| {
                            h["command"].as_str().is_some_and(|c| {
                                c.contains(HOOK) && c.contains("$CLAUDE_PROJECT_DIR")
                            })
                        })
                    })
            }),
            "a {event} group runs {HOOK} after Bash: {after:?}"
        );
    }
}

// ---- after the call: what it actually did to the shared checkout ----
//
// The text checks above are a net, not a proof: a script's body, a
// `python -c`, an expansion, a `bash -c` string, a subshell's cd, a git
// behind `if`/`!`/`{` — the list of what the parse cannot see is not
// closed. So for an isolated agent's Bash call the hook ALSO pictures the
// shared checkout before the call (HEAD's ref and sha, and every changed
// or untracked path with its size and mtime) and compares after it,
// reading git rather than the command text. A change across the call is
// reported to the agent at once, naming what moved and the repair
// (backlog aebd7439: four writes on 2026-10-19 were found by the builder
// itself or the coordinator, minutes to hours later).

/// A shared checkout in miniature, on main, with `origin/main` where main
/// is and an isolated agent's worktree at `.claude/worktrees/agent-a1b2c3`.
struct Shared {
    tmp: PathBuf,
    saved: PathBuf,
    shared: PathBuf,
    wt: PathBuf,
}

impl Shared {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("shared-checkout-after-{name}"));
        let shared = root.join("boss");
        create_dir(&shared.join("src"));
        std::fs::write(shared.join("src/a.rs"), "fn names_file() {}\n").unwrap();
        std::fs::write(shared.join(".gitignore"), ".claude/*\n").unwrap();
        git(&shared, &["init", "-q", "-b", "main"]);
        git(&shared, &["add", "-A"]);
        git(&shared, &["commit", "-qm", "init"]);
        git(&shared, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(
            &shared,
            &[
                "worktree",
                "add",
                "-q",
                ".claude/worktrees/agent-a1b2c3",
                "-b",
                "wt",
            ],
        );
        let tmp = root.join("tmp");
        create_dir(&tmp);
        let wt = shared.join(".claude/worktrees/agent-a1b2c3");
        let saved = root.join("restored");
        Self {
            tmp,
            saved,
            shared,
            wt,
        }
    }

    /// One hook event for a Bash call `id` of the isolated agent (or, with
    /// `agent` false, of the operator's own session).
    fn event(&self, event: &str, id: &str, agent: bool) -> (i32, String) {
        let mut p: serde_json::Value = serde_json::from_str(&payload(
            agent,
            &self.wt.display().to_string(),
            "Bash",
            serde_json::json!({ "command": "bash /tmp/s/x.sh" }),
        ))
        .unwrap();
        p["hook_event_name"] = event.into();
        p["tool_use_id"] = id.into();
        run_hook_in(
            &p.to_string(),
            &self.tmp,
            &[("BOSS_RESTORE_SAVE", &self.saved)],
        )
    }

    fn before(&self, id: &str) {
        let (code, err) = self.event("PreToolUse", id, true);
        assert_eq!(code, 0, "the call itself is not refused: {err}");
    }

    fn after(&self, id: &str) -> (i32, String) {
        self.event("PostToolUse", id, true)
    }

    fn rev(&self, rev: &str) -> String {
        let mut cmd = Command::new("git");
        git_config_isolated(&mut cmd);
        let out = cmd
            .current_dir(&self.shared)
            .args(["rev-parse", rev])
            .output()
            .expect("git rev-parse");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn sha(&self) -> String {
        self.rev("HEAD")
    }

    /// origin/main one commit ahead of the checkout — what a fetch leaves
    /// after a train lands, before the hourly fast-forward takes it.
    fn land_on_origin_main(&self) -> String {
        git(
            &self.wt,
            &["commit", "-q", "--allow-empty", "-m", "a train landed"],
        );
        git(
            &self.shared,
            &["update-ref", "refs/remotes/origin/main", "wt"],
        );
        self.rev("refs/remotes/origin/main")
    }
}

/// The incident of 2026-10-20 ~03:30Z, by any route the text check
/// cannot see: the call returns with the shared checkout on a car branch,
/// or detached.
#[test]
fn a_call_that_moves_the_shared_checkout_off_main_is_reported_when_it_returns() {
    let f = Shared::new("moved");
    f.before("t1");
    git(&f.shared, &["checkout", "-q", "-b", "fix/a-car"]);
    let (code, err) = f.after("t1");
    assert_eq!(code, 2, "a moved shared checkout is loud: {err}");
    for names in [
        "refs/heads/main",
        "refs/heads/fix/a-car",
        "git checkout main",
    ] {
        assert!(err.contains(names), "the report names {names:?}: {err}");
    }

    f.before("t2");
    git(&f.shared, &["checkout", "-q", "--detach"]);
    let (code, err) = f.after("t2");
    assert_eq!(code, 2, "a detached shared checkout is loud: {err}");
    assert!(err.contains("detached"), "{err}");
}

/// A COMMIT on the shared checkout's own branch (review e5607585, finding
/// 2). The first after-call check compared only the ref HEAD names, so a
/// call that wrote a file and ran `git commit -am` — as a script body,
/// `bash -c`, or `git -C /work/boss` would — returned rc 0 and said
/// nothing; the sidecar then logs `ff_result=diverged` and every door
/// judges a diverged checkout not stale. A new sha on the same branch is
/// reported unless origin/main holds it AND it moved forward: that is the
/// hourly fast-forward, and nothing else.
#[test]
fn a_call_that_commits_on_the_shared_checkout_is_reported() {
    let f = Shared::new("commit");
    let before = f.sha();
    f.before("t1");
    std::fs::write(f.shared.join("src/a.rs"), "pub(crate) fn names_file() {}\n").unwrap();
    git(&f.shared, &["commit", "-q", "-am", "a stray commit"]);
    let (code, err) = f.after("t1");
    assert_eq!(code, 2, "a commit on the shared checkout is loud: {err}");
    for names in [&before[..8], "origin/main", "refs/heads/main"] {
        assert!(err.contains(names), "the report names {names:?}: {err}");
    }

    // Backwards counts too: a reset to an older commit origin/main holds
    // is not a fast-forward, and it took whatever was on top with it.
    let f = Shared::new("reset");
    git(
        &f.shared,
        &["commit", "-q", "--allow-empty", "-m", "second"],
    );
    git(
        &f.shared,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    f.before("t1");
    git(&f.shared, &["reset", "-q", "--hard", "HEAD~1"]);
    let (code, err) = f.after("t1");
    assert_eq!(code, 2, "a reset backwards on main is loud: {err}");
}

/// A file written there — tracked, new, or one already dirty written
/// AGAIN (2026-10-19: the third incident's follow-up sed landed on a file
/// the first had already changed) — is named, with the door that puts
/// it back.
#[test]
fn a_call_that_writes_the_shared_checkout_is_reported_with_the_file_and_the_repair() {
    let f = Shared::new("writes");
    f.before("t1");
    std::fs::write(f.shared.join("src/a.rs"), "pub(crate) fn names_file() {}\n").unwrap();
    let (code, err) = f.after("t1");
    assert_eq!(code, 2, "a tracked file changed: {err}");
    assert!(
        err.contains("src/a.rs") && err.contains("restore-shared"),
        "{err}"
    );

    f.before("t2");
    let mut grown = std::fs::read_to_string(f.shared.join("src/a.rs")).unwrap();
    grown.push_str("fn more() {}\n");
    std::fs::write(f.shared.join("src/a.rs"), grown).unwrap();
    let (code, err) = f.after("t2");
    assert_eq!(code, 2, "an already-dirty file written again: {err}");
    assert!(err.contains("src/a.rs"), "{err}");

    f.before("t3");
    std::fs::write(f.shared.join("src/new.rs"), "x\n").unwrap();
    let (code, err) = f.after("t3");
    assert_eq!(code, 2, "a new file: {err}");
    assert!(err.contains("src/new.rs"), "{err}");

    // A call that exits nonzero ends in PostToolUseFailure, and is read
    // the same way.
    f.before("t4");
    std::fs::write(f.shared.join("src/new.rs"), "y, longer\n").unwrap();
    let (code, err) = f.event("PostToolUseFailure", "t4", true);
    assert_eq!(code, 2, "a failed call that wrote: {err}");
    assert!(err.contains("src/new.rs"), "{err}");
}

/// A change DISCARDED during the call is somebody's work gone (review
/// e5607585, finding 3): an uncommitted edit already present before the
/// call — the operator's, or another session's — wiped by `git checkout
/// -- .` or `git stash` inside a script, returned rc 0, because a path
/// that became clean was read as a repair. It is a repair only when
/// infra/dev/restore-shared saved what it discarded, which it does
/// before it touches anything.
#[test]
fn a_call_that_discards_a_change_it_did_not_save_is_reported() {
    let f = Shared::new("discards");
    std::fs::write(f.shared.join("src/a.rs"), "the operator's edit\n").unwrap();
    f.before("t1");
    git(&f.shared, &["checkout", "--", "."]);
    let (code, err) = f.after("t1");
    assert_eq!(code, 2, "a discarded edit is loud: {err}");
    assert!(
        err.contains("discarded") && err.contains("src/a.rs"),
        "{err}"
    );

    std::fs::write(f.shared.join("src/a.rs"), "a stray write\n").unwrap();
    f.before("t2");
    let mut door = Command::new(repo_root().join(DOOR));
    git_config_isolated(&mut door);
    let out = door
        .env("BOSS_SHARED_CHECKOUT", &f.shared)
        .env("BOSS_RESTORE_SAVE", &f.saved)
        .arg("src/a.rs")
        .output()
        .expect("run the door");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (code, err) = f.after("t2");
    assert_eq!(
        code, 0,
        "restore-shared saved what it discarded, so it is a repair: {err}"
    );
}

/// What it does not report: a call that changed only its own worktree; the
/// reclaim sidecar's hourly fast-forward of main to a commit origin/main
/// holds, which a twelve-minute `boss gate --wait` spans often; its return
/// of a clean checkout to main; a call it has no before-picture of; the
/// operator's own session.
#[test]
fn a_call_that_leaves_the_shared_checkout_alone_reports_nothing() {
    let f = Shared::new("alone");
    f.before("t1");
    std::fs::write(f.wt.join("src/a.rs"), "pub(crate) fn names_file() {}\n").unwrap();
    let landed = f.land_on_origin_main();
    git(&f.shared, &["merge", "-q", "--ff-only", &landed]);
    assert_eq!(f.sha(), landed);
    let (code, err) = f.after("t1");
    assert_eq!(code, 0, "a fast-forward is the sidecar working: {err}");

    git(&f.shared, &["checkout", "-q", "--detach"]);
    f.before("t2");
    git(&f.shared, &["checkout", "-q", "main"]);
    let (code, err) = f.after("t2");
    assert_eq!(code, 0, "a return to main is a repair: {err}");

    let (code, err) = f.after("never-pictured");
    assert_eq!(code, 0, "{err}");

    std::fs::write(f.shared.join("src/a.rs"), "the operator's edit\n").unwrap();
    assert_eq!(f.event("PreToolUse", "t3", false).0, 0);
    std::fs::write(f.shared.join("src/a.rs"), "the operator's edit, again\n").unwrap();
    let (code, err) = f.event("PostToolUse", "t3", false);
    assert_eq!(code, 0, "the operator's session is never judged: {err}");
}

/// A before-picture whose call never ends — denied by permission or by
/// another hook, or its session killed — is removed by a later call
/// once it is a day old (review e5607585, finding 6: nothing reaped them).
#[test]
fn a_picture_whose_call_never_ended_is_reaped() {
    let f = Shared::new("reap");
    f.before("t1");
    let dir = std::fs::read_dir(&f.tmp)
        .expect("the picture directory")
        .next()
        .expect("one picture directory")
        .unwrap()
        .path();
    let stale = dir.join("a1b2c3-orphan.json");
    std::fs::write(&stale, "{}").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86400);
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let fresh = dir.join("a1b2c3-t1.json");
    assert!(
        fresh.exists(),
        "the live call's picture: {}",
        fresh.display()
    );
    f.before("t2");
    assert!(!stale.exists(), "a day-old picture is reaped");
    assert!(fresh.exists(), "a live call's picture is kept");
}

// ---- the repair door ----

struct Repo {
    root: PathBuf,
    shared: PathBuf,
    saved: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    git_config_isolated(&mut cmd);
    let out = cmd
        .current_dir(dir)
        .args(["-c", "user.email=t@test", "-c", "user.name=test"])
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

impl Repo {
    /// A main checkout with one tracked file and a linked worktree under
    /// `.claude/worktrees/` — the shared checkout's shape.
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("restore-shared-{name}"));
        let shared = root.join("shared");
        create_dir(&shared.join("src"));
        std::fs::write(shared.join("src/a.rs"), "fn names_file() {}\n").unwrap();
        std::fs::write(shared.join(".gitignore"), ".claude/worktrees/\n").unwrap();
        git(&shared, &["init", "-q", "-b", "main"]);
        git(&shared, &["add", "-A"]);
        git(&shared, &["commit", "-qm", "init"]);
        git(
            &shared,
            &[
                "worktree",
                "add",
                "-q",
                ".claude/worktrees/agent-x",
                "-b",
                "wt",
            ],
        );
        let saved = root.join("saved");
        Self {
            root,
            shared,
            saved,
        }
    }

    fn door(&self, args: &[&str]) -> (i32, String, String) {
        let mut cmd = Command::new(repo_root().join(DOOR));
        git_config_isolated(&mut cmd);
        let out = cmd
            .current_dir(&self.root)
            .env("BOSS_SHARED_CHECKOUT", &self.shared)
            .env("BOSS_RESTORE_SAVE", &self.saved)
            .args(args)
            .output()
            .expect("run the door");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// The incident's repair: the file goes back to HEAD, and the change it
/// discards is kept, not destroyed — by a relative path or an absolute one.
#[test]
fn the_door_restores_a_file_just_dirtied_and_keeps_what_it_discarded() {
    let r = Repo::new("restores");
    let file = r.shared.join("src/a.rs");
    for arg in ["src/a.rs".to_string(), file.display().to_string()] {
        std::fs::write(&file, "pub(crate) fn names_file() {}\n").unwrap();
        let (code, out, err) = r.door(&[&arg]);
        assert_eq!(code, 0, "{out}{err}");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "fn names_file() {}\n"
        );
        let patches: Vec<String> = std::fs::read_dir(&r.saved)
            .expect("the saved directory")
            .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
            .collect();
        assert!(
            patches
                .iter()
                .any(|p| p.contains("+pub(crate) fn names_file")),
            "the discarded change is saved: {patches:?}"
        );
        assert!(out.contains("restored src/a.rs"), "{out}");
        for p in std::fs::read_dir(&r.saved).unwrap() {
            std::fs::remove_file(p.unwrap().path()).unwrap();
        }
    }
}

/// What the door refuses, each by name: an untracked file, a clean one,
/// a path in a worktree or outside the checkout, and a change older than
/// the window — the operator's own edit is not a builder's stray one.
#[test]
fn the_door_refuses_what_it_cannot_know_a_builder_dirtied() {
    let r = Repo::new("refuses");
    std::fs::write(r.shared.join("src/new.rs"), "x").unwrap();
    let (code, _, err) = r.door(&["src/new.rs"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("not tracked"), "{err}");

    let (code, _, err) = r.door(&["src/a.rs"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("no change"), "{err}");

    let (code, _, err) = r.door(&[".claude/worktrees/agent-x/src/a.rs"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("worktree"), "{err}");

    let (code, _, err) = r.door(&["/etc/hostname"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("outside"), "{err}");

    let file = r.shared.join("src/a.rs");
    std::fs::write(&file, "operator's own edit\n").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let (code, _, err) = r.door(&["src/a.rs"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("older than"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "operator's own edit\n",
        "a refused restore touches nothing"
    );

    let (code, _, err) = r.door(&[]);
    assert_eq!(code, 2, "{err}");
}

/// A DIRECTORY is refused, and so is a pathspec (review finding 1). A
/// builder's stray write inside a directory freshens the DIRECTORY's
/// mtime, so a window judged on it passed, and `git checkout HEAD --
/// <dir>` then reverted every tracked change under it — measured, an
/// operator's two-hour-old edit included. The window is a FILE's.
#[test]
fn the_door_refuses_a_directory_and_a_pathspec_and_touches_nothing() {
    let r = Repo::new("directory");
    let op = r.shared.join("src/a.rs");
    std::fs::write(&op, "operator's own edit\n").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
    std::fs::File::options()
        .write(true)
        .open(&op)
        .unwrap()
        .set_modified(old)
        .unwrap();
    // A builder's fresh write beside it freshens src/.
    std::fs::write(r.shared.join("src/stray.tmp"), "x").unwrap();
    for arg in ["src", "src/", "src/*", ":(glob)src/**"] {
        let (code, _, err) = r.door(&[arg]);
        assert_eq!(code, 1, "{arg}: {err}");
        assert_eq!(
            std::fs::read_to_string(&op).unwrap(),
            "operator's own edit\n",
            "{arg}: the operator's older edit is untouched"
        );
    }
    let (_, _, err) = r.door(&["src"]);
    assert!(err.contains("not a regular file"), "{err}");
}

/// A binary change is saved so that it can be put back (review finding
/// 2): saved without --binary it was the one line "Binary files a/x and
/// b/x differ", which passed the non-empty check while the checkout
/// destroyed the content for good.
#[test]
fn the_door_saves_a_binary_change_so_it_can_be_put_back() {
    let r = Repo::new("binary");
    let blob = r.shared.join("src/blob.bin");
    std::fs::write(&blob, [0u8, 1, 2, 3, 0, 255]).unwrap();
    git(&r.shared, &["add", "src/blob.bin"]);
    git(&r.shared, &["commit", "-qm", "blob"]);
    let changed = [0u8, 9, 9, 9, 0, 254, 7];
    std::fs::write(&blob, changed).unwrap();
    let (code, out, err) = r.door(&["src/blob.bin"]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(std::fs::read(&blob).unwrap(), [0u8, 1, 2, 3, 0, 255]);
    let patch = std::fs::read_dir(&r.saved)
        .expect("the saved directory")
        .next()
        .expect("one saved patch")
        .unwrap()
        .path();
    git(&r.shared, &["apply", &patch.display().to_string()]);
    assert_eq!(
        std::fs::read(&blob).unwrap(),
        changed,
        "the saved patch puts the binary change back"
    );
}
