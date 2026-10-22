#!/usr/bin/python3 -I
"""PreToolUse guard: a worktree-isolated agent does not write the shared checkout.

Backlog 10d1567b, 2026-10-10. Builder run 63c60863 ran `cd <its worktree>
&& sed -i ...`; the harness dropped the cd (measured, backlog 916fe290:
every Bash call of an isolated agent starts in the dispatching session's
checkout, /work/boss, and a leading `cd <worktree> &&` never reaches
bash), so the sed edited two source files in /work/boss. Claude Code's
own isolation guard reads only git: it let the sed through and then
REFUSED the builder's `git -C /work/boss checkout --` repair. This hook
refuses the damage instead; infra/dev/restore-shared is the repair.

WHAT IT SEES (measured on Claude Code 2.1.296, scratch projects): a
subagent's payload carries `agent_id` and `cwd`; an ISOLATED agent's cwd
is its own worktree, `<shared>/.claude/worktrees/agent-<agent_id>` (review
0dc61dd6's dump: agent_id a42a9dceadb21aa39, cwd ...agent-a42a9dceadb21aa39).
Agent-definition frontmatter `hooks:` did not fire, so this is registered
in .claude/settings.json and scoped HERE, to that one shape. The
operator's session (no agent_id), an agent at <shared>, and a
NON-isolated subagent of a session that itself works in a worktree (its
cwd names no `agent-<its id>`) are never judged.

WHERE A CALL RUNS IS NOT THE SAME ON EVERY HARNESS PATH, so it judges a
path AS WRITTEN, from BOTH places it may run. On the production dispatch
916fe290 measured (2026-10-09, run 171055b8) an isolated agent's Bash
started in /work/boss and a leading `cd <worktree> &&` was dropped
before bash. Under `claude -p` review 0dc61dd6 measured (2026-10-10) it
started in the worktree, a relative `sed -i` landed there, and the cd
was stripped BEFORE this hook saw it. Nothing in the payload says which
path it is on. Until backlog 88553fea a bare relative target therefore
PASSED, and on 2026-10-20 that hole was the whole incident: two mutation
seds (run 472b2b58) dirtied /work/boss, and three reviewers' bare `git
checkout`/`git switch` moved it off main — once for thirteen minutes,
while every pod door ran another agent's branch. Builder run 7934069e
re-measured the production path the same day: its payload's cwd named
its worktree while `pwd -P` printed /work/boss. So until a `cd` with an
absolute target takes effect, a relative path is resolved from the
reported cwd AND from <shared>, and refused if either lands in <shared>
outside the worktree. On `claude -p` that refuses a write that would
have landed safely; the way out is one retry, the miss cost the doors.

WHAT IT REFUSES (exit 2, the reason on stderr for the model):
  * the shape the production path drops: a command that opens
    `cd <worktree> &&` (it fires only where the hook still sees the cd);
  * a write whose target, as written, lands in <shared> outside the
    caller's OWN worktree — an absolute path, a `../` climbing out, a
    bare relative path (from <shared>, above), or a relative path after
    a `cd` — including another agent's worktree and the worktrees
    directory itself: a redirection, or a writer's file argument (sed
    -i, perl -i, tee, cp, mv, rm, touch, truncate, mkdir, ln, install,
    chmod, patch, dd of=, cargo fmt);
  * a git that moves a tree — checkout, switch, reset, restore, stash
    (but `stash list`/`show`), commit, merge, pull, rebase, cherry-pick,
    revert, am, clean — whose tree (`-C`, after any `cd`) is not named
    as the caller's own worktree (backlog 88553fea). The harness's own
    guard refuses `git -C /work/boss`; it never saw a bare git that RAN
    there;
  * Write / Edit / NotebookEdit on such a path.
It parses text, so it is a net, not a proof. What it does NOT see, and
passes: a target written through an expansion (`$VAR`, `$(...)`, a
backtick), a script's body, `python -c`, `xargs`, `find -exec`, `curl
-o`, `wget -O`, `awk -i inplace`, `rustfmt`, `tar -x`/`-C`, git's
`--git-dir`/`--work-tree`/`GIT_DIR`, and cargo or bun writing into the
cwd they run in (target/, Cargo.lock, node_modules). The list is not
closed. Text it cannot parse refuses nothing — a guard that refuses
because it cannot SEE is worse than the miss it prevents
(shared_checkout.rs, 916fe290).

Pinned by crates/core/boss-testing/tests/shared_checkout_guard.rs.
"""
import json
import os
import re
import shlex
import sys

WORKTREES = "/.claude/worktrees/"
REDIRECTS = {">", ">>", ">|", "&>", "&>>", ">&"}
SEPARATORS = set("();|&\n")
WRAPPERS = {"sudo", "env", "nice", "nohup", "time", "command", "exec", "stdbuf"}
# writer -> how its positional arguments name what it writes
ALL, LAST, AFTER_FIRST = "all", "last", "after-first"
WRITERS = {
    "tee": ALL, "rm": ALL, "touch": ALL, "truncate": ALL, "mkdir": ALL,
    "rmdir": ALL, "unlink": ALL, "mv": ALL, "shred": ALL,
    "cp": LAST, "ln": LAST, "install": LAST, "rsync": LAST,
    "chmod": AFTER_FIRST, "chown": AFTER_FIRST, "chgrp": AFTER_FIRST,
}
OPTS_WITH_ARG = {"truncate": {"-s", "--size", "-r", "--reference"}, "install": {"-m", "-o", "-g"}}
# git subcommands that move HEAD, the index or the work tree (88553fea).
GIT_MOVERS = {"checkout", "switch", "reset", "restore", "stash", "commit", "merge", "pull",
              "rebase", "cherry-pick", "revert", "am", "clean"}
GIT_STASH_READS = {"list", "show"}


def refuse(text):
    sys.stderr.write(text.rstrip() + "\n")
    sys.exit(2)


def within(path, root):
    return path == root or path.startswith(root.rstrip("/") + "/")


def lexed(command):
    """Tokens of `command`, heredoc bodies dropped, newlines kept as separators."""
    command = command.replace("\\\n", " ")
    kept, closer = [], None
    for line in command.split("\n"):
        if closer is not None:
            if line.strip() == closer:
                closer = None
            continue
        kept.append(line)
        m = re.search(r"<<-?\s*['\"]?([A-Za-z_][A-Za-z0-9_]*)['\"]?", line.replace("<<<", ""))
        if m:
            closer = m.group(1)
    # A newline separates commands as `;` does, so it is punctuation here
    # and not whitespace.
    lex = shlex.shlex("\n".join(kept), posix=True, punctuation_chars="();<>|&\n")
    lex.whitespace = " \t\r"
    lex.whitespace_split = True
    return list(lex)


def simple_commands(tokens):
    """Split into (separator-before, words) pairs."""
    out, words, sep = [], [], ""
    for tok in tokens:
        if tok and tok not in REDIRECTS and all(c in SEPARATORS for c in tok) and ">" not in tok:
            if words:
                out.append((sep, words))
            words, sep = [], tok
        else:
            words.append(tok)
    if words:
        out.append((sep, words))
    return out


def targets(words):
    """(command name, the paths this simple command writes)."""
    written, rest, i = [], [], 0
    while i < len(words):
        w = words[i]
        if w in REDIRECTS:
            nxt = words[i + 1] if i + 1 < len(words) else ""
            if not (w == ">&" and re.fullmatch(r"\d+|-", nxt)):
                written.append(nxt)
            i += 2
            continue
        if w.isdigit() and i + 1 < len(words) and words[i + 1] in REDIRECTS:
            i += 1
            continue
        # An INPUT redirection and its source are read, never written: as
        # a bare word `<` would land in a writer's arguments, and once a
        # bare relative target is judged from <shared> (88553fea) `tee x
        # < /dev/null` reads as a write of /work/boss/<.
        if w and set(w) <= {"<"}:
            i += 2
            continue
        rest.append(w)
        i += 1
    while rest and (re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", rest[0]) or os.path.basename(rest[0]) in WRAPPERS):
        rest = rest[1:]
    if not rest:
        return "", written
    name, args = os.path.basename(rest[0]), rest[1:]
    if name == "git":
        # What a tree-moving git writes is the tree it runs in: `.`,
        # joined with each -C in turn — so a bare one is judged from
        # both bases like any other relative path (88553fea).
        tree, i = ".", 0
        while i < len(args) and args[i].startswith("-"):
            if args[i] == "-C" and i + 1 < len(args):
                tree = os.path.join(tree, args[i + 1])
            i += 2 if args[i] in ("-C", "-c") else 1
        sub = args[i] if i < len(args) else ""
        if sub in GIT_MOVERS and not (sub == "stash" and args[i + 1:i + 2] and args[i + 1] in GIT_STASH_READS):
            return f"git {sub}", written + [tree]
        return name, written
    if name in ("cargo", "wt-cargo") and args[:1] == ["fmt"]:
        written.append(".")
    elif name in ("sed", "perl"):
        in_place = any(re.match(r"^-[A-Za-z]*i", a) or a.startswith("--in-place") for a in args)
        if in_place:
            given = any(a in ("-e", "--expression", "-f", "--file") or a.startswith("--expression=")
                        or re.match(r"^-[A-Za-z]*e$", a) for a in args)
            pos, skip = [], False
            for a in args:
                if skip:
                    skip = False
                    continue
                if a in ("-e", "--expression", "-f", "--file") or re.match(r"^-[A-Za-z]*e$", a):
                    skip = True
                    continue
                if a.startswith("-") and a != "-":
                    continue
                pos.append(a)
            written += pos if given else pos[1:]
    elif name == "dd":
        written += [a[3:] for a in args if a.startswith("of=")]
    elif name == "patch":
        pos = [a for a in args if not a.startswith("-")]
        written += pos[:1] or ["."]
    elif name in WRITERS:
        pos, take = [], None
        with_arg = OPTS_WITH_ARG.get(name, set())
        for a in args:
            if take is not None:
                if take:
                    written.append(a)
                take = None
            elif a in ("-t", "--target-directory"):
                take = True
            elif a.startswith("--target-directory="):
                written.append(a.split("=", 1)[1])
            elif a in with_arg:
                take = False
            elif not (a.startswith("-") and a != "-"):
                pos.append(a)
        mode = WRITERS[name]
        written += {ALL: pos, LAST: pos[-1:], AFTER_FIRST: pos[1:]}.get(mode, [])
    return name, [t for t in written if t]


def main():
    try:
        payload = json.loads(sys.stdin.read())
    except Exception:
        return 0
    if not isinstance(payload, dict) or not payload.get("agent_id"):
        return 0
    cwd = os.path.normpath(str(payload.get("cwd") or "/"))
    if WORKTREES not in cwd + "/":
        return 0
    shared, below = (cwd + "/").split(WORKTREES, 1)
    shared = shared or "/"
    # Only the agent's OWN isolated worktree: a subagent that merely
    # inherits a worktree cwd from its session is not isolated (review
    # 0dc61dd6, finding 3), and only this one tree is exempt — not a
    # sibling agent's, not the directory holding them all (finding 6).
    if below.split("/", 1)[0] != f"agent-{payload['agent_id']}":
        return 0
    worktree = shared.rstrip("/") + WORKTREES + below.split("/", 1)[0]

    def into_shared(path, base):
        full = os.path.normpath(os.path.join(base, os.path.expanduser(path)))
        # An expansion — in the target, or in a `cd` before it — hides
        # where it lands; refuse nothing it cannot see (finding 5).
        if "$" in full or "`" in full:
            return False, full
        return within(full, shared) and not within(full, worktree), full

    ways = (
        f"Name the file by its absolute worktree path ({worktree}/...), use git -C {worktree} "
        f"for git, or write the commands into a script whose first line is `cd {worktree} || exit 1` "
        f"and run that script. If an earlier command already dirtied {shared}, put the file back "
        f"with `{shared}/infra/dev/restore-shared <file>...` — one tracked file per argument, never "
        f"a directory; it saves the discarded change first."
    )
    tool = payload.get("tool_name")
    tin = payload.get("tool_input")
    if not isinstance(tin, dict):
        return 0
    if tool in ("Write", "Edit", "NotebookEdit"):
        path = str(tin.get("file_path") or tin.get("notebook_path") or "")
        hit, full = into_shared(path, worktree) if path else (False, "")
        if hit:
            refuse(f"shared-checkout guard (backlog 10d1567b): {tool} on {full} writes the shared "
                   f"checkout {shared}, outside your worktree {worktree}. {ways}")
        return 0
    if tool != "Bash":
        return 0
    command = str(tin.get("command") or "")
    try:
        commands = simple_commands(lexed(command))
    except ValueError:
        return 0
    # Judged from BOTH places the call may run — the cwd the payload
    # reports and the shared checkout — until a cd names one; see WHERE A
    # CALL RUNS. A cd with an absolute target collapses them to one.
    bases = [cwd, shared]
    for n, (sep, words) in enumerate(commands):
        if words and words[0] == "cd":
            dest = words[1] if len(words) > 1 else os.path.expanduser("~")
            target = os.path.normpath(os.path.join(cwd, dest))
            nxt = commands[n + 1][0] if n + 1 < len(commands) else ""
            if n == 0 and nxt == "&&" and target in (cwd, worktree):
                refuse(f"shared-checkout guard (backlog 10d1567b): the harness drops a leading "
                       f"`cd {dest} &&`, so this command would run in {shared}, the shared checkout. "
                       f"{ways}")
            bases = list(dict.fromkeys(os.path.normpath(os.path.join(b, dest)) for b in bases))
            continue
        name, written = targets(words)
        for path in written:
            if path.startswith("/dev/"):
                continue
            for base in bases:
                hit, full = into_shared(path, base)
                if not hit:
                    continue
                if name.startswith("git "):
                    refuse(f"shared-checkout guard (backlog 88553fea): `{name}` would move {full}, "
                           f"the shared checkout every pod door runs from (/work/tools/bin links into "
                           f"its infra/dev), and not your worktree {worktree}: a bare git runs in "
                           f"{shared} when the harness drops your cd. Name the tree: git -C {worktree} "
                           f"{name[4:]} ... If {shared} is already off main, say so in your report; the "
                           f"dev pod's reclaim pass returns a clean one to main hourly.")
                refuse(f"shared-checkout guard (backlog 10d1567b): `{name or 'a redirection'}` would "
                       f"write {full}, in the shared checkout {shared} and outside your worktree "
                       f"{worktree}. {ways}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
