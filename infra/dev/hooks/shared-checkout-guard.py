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
  * an interpreter's INLINE program — python `-c`, perl/ruby/node/bun
    `-e`, or one read from stdin (`-`, a heredoc, a here-string) — that
    carries a write call (`open(p,'w')`, `.write_text(`, `os.replace(`,
    `writeFileSync(`, `Bun.write(`, perl's `open(F, '>' ...)`), when a
    path it names lands in <shared> outside the worktree, or when no
    absolute `cd` has fixed where it runs and it names a relative path or
    no absolute one at all (backlog ab0702d2: twice on 2026-10-22, runs
    adc74f96 and 2f9b5353, `cd <worktree> && python3 -I - <<'PY'`
    reached this hook with the cd already stripped, and its
    `open(p,'w')` of a relative `p` rewrote a source file in <shared>).
    A program that only reads, or writes only by absolute path outside
    <shared>, passes, and so does one whose write call is not in that
    list: that is the `python -c` the paragraph below still names;
  * a git that moves a tree — checkout, switch, reset, restore, stash
    (but `stash list`/`show`), commit, merge, pull, rebase, cherry-pick,
    revert, am, clean — whose tree (`-C`, after any `cd`) is not named
    as the caller's own worktree (backlog 88553fea). The harness's own
    guard refuses `git -C /work/boss`; it never saw a bare git that RAN
    there. A tree named by `--git-dir`, `--work-tree`, `GIT_DIR=` or
    `GIT_WORK_TREE=` is judged with it (review e5607585, finding 4: each
    passed, and nothing in the tree showed the harness refusing them);
  * Write / Edit / NotebookEdit on such a path.
It parses text, so it is a net, not a proof. What it does NOT see, and
passes: a target written through an expansion (`$VAR`, `$(...)`, a
backtick), a script's body, `bash -c`, `python -c`, `xargs`, `find
-exec`, `curl -o`, `wget -O`, `awk -i inplace`, `rustfmt`, `tar -x`/`-C`,
a `cd` inside a subshell (read as if it persisted), a git behind `if`,
`!`, `{` or `do`, and cargo or bun writing into the cwd they run in
(target/, Cargo.lock, node_modules). The list is not closed. Text it
cannot parse refuses nothing — a guard that refuses because it cannot
SEE is worse than the miss it prevents (shared_checkout.rs, 916fe290).

AND AFTER THE CALL IT READS GIT, NOT TEXT (backlog aebd7439), which is
what catches the list above. For an isolated agent's Bash call that it
passed, PreToolUse pictures <shared> — HEAD's ref and sha, and every
changed or untracked path with its size and mtime — and PostToolUse
(PostToolUseFailure for a call that exited nonzero) compares. Reported
to the agent at once (exit 2, which both events show the model), naming
what moved and the repair:
  * a HEAD that moved, unless it is now `main` at a commit origin/main
    holds and, if it was on main before, moved FORWARD to it — the reclaim
    sidecar's hourly fast-forward and its return of a clean checkout to
    main. So a commit made there is reported (review e5607585, finding 2:
    the first version compared only the ref name, and `git commit -am`
    inside a script returned silent, leaving /work/boss diverged where
    no door judges it stale), and so is a reset backwards;
  * a path written during the call (dirty after it, and not as it was);
  * a path that was dirty before and is not after — somebody's change
    discarded or committed — unless infra/dev/restore-shared saved it
    during the call, which is the repair (finding 3: a `git checkout --
    .` or `git stash` wiping another session's edit read as a repair).
A call it has no before-picture of, or a <shared> git cannot read,
reports nothing. A picture whose call never ended is removed by a later
call once a day old (finding 6).

Pinned by crates/core/boss-testing/tests/shared_checkout_guard.rs.
"""
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
import time

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
# An interpreter run on an inline program, and the flag that hands it one
# (ab0702d2). Its text is not parsed: a write call in it is the signal.
INTERPRETER = re.compile(r"python[0-9.]*|perl|ruby|node|bun")
INLINE_FLAG = {"python": re.compile(r"-[A-Za-z]*c"), "perl": re.compile(r"-[A-Za-z]*[eE]"),
               "ruby": re.compile(r"-[A-Za-z]*e"), "js": re.compile(r"-[A-Za-z]*[ep][A-Za-z]*|--eval|--print")}
PY_OPTS_WITH_ARG = {"-X", "-W"}
WRITE_CALL = re.compile(r"""
    \bopen\s*\((?:[^()]|\([^()]*\))*?,\s*(?:mode\s*=\s*)?['"][rbt]*[wax+][rbt+]*['"]
  | \bopen\b[^;\n]*?['"]\s*\+?>
  | \.write_(?:text|bytes)\s*\(
  | \b(?:os|shutil)\.(?:rename|replace|remove|unlink|mkdir|makedirs|rmdir|truncate|copy\w*|move|rmtree)\s*\(
  | \.(?:unlink|touch|mkdir|rmdir)\s*\(
  | \b(?:writeFile|appendFile|rm|unlink|rename|mkdir|copyFile|truncate)(?:Sync)?\s*\(
  | \bcreateWriteStream\s*\( | \bBun\.write\s*\( | \bFile\.(?:write|delete|rename)\b | \bFileUtils\.
""", re.X)
QUOTED = re.compile(r"""(['"])([^'"\s]+)\1""")
# Only literal targets in WRITE position can establish a safe absolute
# destination (review a008012c). Other languages/shapes need an explicit
# cwd; an absolute input elsewhere in the program proves nothing.
DIRECT_WRITE_TARGET = re.compile(r"""^(?:open|writeFile(?:Sync)?|appendFile(?:Sync)?|rm(?:Sync)?|unlink(?:Sync)?|mkdir(?:Sync)?|truncate(?:Sync)?|createWriteStream|Bun\.write|File\.(?:write|delete)|os\.(?:remove|unlink|mkdir|makedirs|rmdir|truncate))\s*\(\s*(['"])([^'"]+)\1\s*[,)]""")
PATH_WRITE_TARGET = re.compile(r"""\b(?:pathlib\.)?Path\s*\(\s*(['"])([^'"]+)\1\s*\)\s*$""")
# git subcommands that move HEAD, the index or the work tree (88553fea).
GIT_MOVERS = {"checkout", "switch", "reset", "restore", "stash", "commit", "merge", "pull",
              "rebase", "cherry-pick", "revert", "am", "clean"}
GIT_STASH_READS = {"list", "show"}
# git's global options that take their value as the next word.
GIT_OPTS_WITH_ARG = {"-C", "-c", "--git-dir", "--work-tree", "--namespace", "--config-env"}
# Each names a tree a moving git writes, as -C does (finding 4).
GIT_TREE_OPTS = {"--git-dir", "--work-tree"}
GIT_TREE_ENV = ("GIT_DIR=", "GIT_WORK_TREE=")
# How long a before-picture may wait for its call's end (finding 6).
PICTURE_DAYS = 1


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


def inline_program(words, command):
    """(interpreter, program text, the words after it) when this simple
    command runs an interpreter on an inline program, else None. A program
    read from stdin is the whole command's text, heredoc body and all."""
    rest, i = [], 0
    while i < len(words):
        w = words[i]
        if w in REDIRECTS or (w and set(w) <= {"<"}):
            i += 2
            continue
        rest.append(w)
        i += 1
    while rest and (re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", rest[0]) or os.path.basename(rest[0]) in WRAPPERS):
        rest = rest[1:]
    name = os.path.basename(rest[0]) if rest else ""
    if not INTERPRETER.fullmatch(name):
        return None
    python = name.startswith("python")
    flag = INLINE_FLAG["python" if python else name if name in INLINE_FLAG else "js"]
    args, j = rest[1:], 0
    while j < len(args):
        a = args[j]
        if flag.fullmatch(a):
            return (name, args[j + 1], args[j + 2:]) if j + 1 < len(args) else None
        if a == "-":
            return name, command, args[j + 1:]
        if python and a == "-m":
            return None
        if not a.startswith("-"):
            return None
        j += 2 if python and a in PY_OPTS_WITH_ARG else 1
    return name, command, []


def inline_write_targets(program):
    """Literal targets for EVERY recognized write, or None when any
    target is computed/unknown. Do not infer targets from read arguments."""
    paths = []
    for call in WRITE_CALL.finditer(program):
        target = DIRECT_WRITE_TARGET.match(program[call.start():])
        if target is None and call.group().startswith("."):
            target = PATH_WRITE_TARGET.search(program[:call.start()])
        if target is None:
            return None
        paths.append(target.group(2))
    return paths


def judge_inline(words, command, bases, into_shared, ways):
    """Refuse an inline program that writes, judged by every path it
    names — quoted in its text or passed after it (ab0702d2). `bases`
    are the places this simple command may run, as main() tracks them."""
    inline = inline_program(words, command)
    if inline is None or not WRITE_CALL.search(inline[1]):
        return
    writes = inline_write_targets(inline[1])
    named = [m.group(2) for m in QUOTED.finditer(inline[1])] + list(inline[2]) + (writes or [])
    absolute = [p for p in named if re.match(r"^~?/.", p)]
    relative = [p for p in named if not re.match(r"^[~/$-]", p) and "://" not in p
                and re.fullmatch(r"[\w.@+-]+/[\w./@+-]*|[\w-][\w.-]*\.[A-Za-z][A-Za-z0-9]{0,5}", p)]
    for path in absolute:
        hit, full = into_shared(path, "/")
        if hit:
            refuse(f"shared-checkout guard (backlog ab0702d2): `{inline[0]}` runs an inline program that "
                   f"writes, and it names {full}, in the shared checkout and outside your worktree. {ways}")
    here = [b for b in bases if into_shared(".", b)[0]]
    known_absolute = writes and all(os.path.isabs(os.path.expanduser(p)) for p in writes)
    if here and not known_absolute:
        what = (f"a relative path ({into_shared(relative[0], here[0])[1]} from {here[0]})" if relative
                else "no known absolute target for every write, so where it writes is not known")
        refuse(f"shared-checkout guard (backlog ab0702d2): `{inline[0]}` runs an inline program that "
               f"writes and names {what}, and this call may run in the shared checkout {here[0]}, not "
               f"your worktree: the harness drops a leading `cd <worktree> &&`, and twice on 2026-10-22 a "
               f"python3 heredoc rewrote a source file there. Edit the file with the Edit tool, or "
               f"{ways[0].lower()}{ways[1:]}")


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
    env = []
    while rest and (re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", rest[0]) or os.path.basename(rest[0]) in WRAPPERS):
        if rest[0].startswith(GIT_TREE_ENV):
            env.append(rest[0].split("=", 1)[1])
        rest = rest[1:]
    if not rest:
        return "", written
    name, args = os.path.basename(rest[0]), rest[1:]
    if name == "git":
        # What a tree-moving git writes is the tree it runs in: `.`,
        # joined with each -C in turn — so a bare one is judged from
        # both bases like any other relative path (88553fea) — and any
        # tree it names by --git-dir, --work-tree, GIT_DIR= or
        # GIT_WORK_TREE= (review e5607585, finding 4).
        tree, named, i = ".", list(env), 0
        while i < len(args) and args[i].startswith("-"):
            opt, given = args[i].split("=", 1) if "=" in args[i] else (args[i], None)
            if given is None and opt in GIT_OPTS_WITH_ARG:
                given = args[i + 1] if i + 1 < len(args) else ""
                i += 1
            if opt == "-C" and given is not None:
                tree = os.path.join(tree, given)
            elif opt in GIT_TREE_OPTS and given:
                named.append(given)
            i += 1
        sub = args[i] if i < len(args) else ""
        if sub in GIT_MOVERS and not (sub == "stash" and args[i + 1:i + 2] and args[i + 1] in GIT_STASH_READS):
            return f"git {sub}", written + [tree] + named
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


def git_in(shared, *args):
    """git on <shared>, bounded so that three calls fit the hook's 10 s."""
    return subprocess.run(["git", "--no-optional-locks", "-C", shared, *args],
                          capture_output=True, timeout=3, check=False)


def picture(shared):
    """<shared> as git sees it in ONE read, or None: HEAD's ref (a
    detached HEAD named by its sha) and sha, and each changed or untracked
    path with its status, size and mtime — so a file already dirty and
    written AGAIN still differs."""
    at = time.time_ns()
    try:
        out = git_in(shared, "status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all")
    except (OSError, subprocess.SubprocessError):
        return None
    if out.returncode != 0:
        return None
    head = sha = None
    paths, entries, i = {}, out.stdout.decode(errors="replace").split("\0"), 0
    while i < len(entries):
        entry = entries[i]
        i += 1
        if entry.startswith("# branch.oid "):
            sha = entry.split(" ", 2)[2]
            continue
        if entry.startswith("# branch.head "):
            head = entry.split(" ", 2)[2]
            continue
        if entry.startswith("? "):
            code, path = "??", entry[2:]
        elif entry[:2] in ("1 ", "2 ", "u "):
            # Ordinary, renamed (its source rides as the next entry),
            # unmerged: the path is the last of a fixed count of fields.
            n = {"1": 8, "2": 9, "u": 10}[entry[0]]
            fields = entry.split(" ", n)
            if len(fields) <= n:
                continue
            code, path = fields[1], fields[n]
            if entry[0] == "2":
                i += 1
        else:
            continue
        try:
            st = os.lstat(os.path.join(shared, path))
            paths[path] = [code, st.st_size, st.st_mtime_ns]
        except OSError:
            paths[path] = [code, None, None]
    if not head or not sha:
        return None
    ref = f"a detached HEAD at {sha[:8]}" if head == "(detached)" else f"refs/heads/{head}"
    return {"head": ref, "sha": sha, "paths": paths, "at": at}


def picture_file(payload):
    key = re.sub(r"[^A-Za-z0-9_.-]", "_", f"{payload['agent_id']}-{payload.get('tool_use_id') or 'call'}")
    return os.path.join(tempfile.gettempdir(), f"boss-shared-guard-{os.getuid()}", key + ".json")


def before_call(payload, shared):
    """Keep the before-picture; a failure here costs only the after-check."""
    try:
        path = picture_file(payload)
        folder = os.path.dirname(path)
        os.makedirs(folder, mode=0o700, exist_ok=True)
        # A picture whose PostToolUse never came — the call denied by a
        # permission or another hook, the session killed — is nobody's
        # after a day (review e5607585, finding 6).
        cutoff = time.time() - PICTURE_DAYS * 86400
        for old in os.scandir(folder):
            if old.is_file(follow_symlinks=False) and old.stat(follow_symlinks=False).st_mtime < cutoff:
                os.unlink(old.path)
        now = picture(shared)
        if now is not None:
            with open(path, "w") as f:
                json.dump(now, f)
    except Exception:
        pass


def is_ancestor(shared, older, newer):
    """True only when git SAYS so; a git that cannot answer is not a yes."""
    try:
        return git_in(shared, "merge-base", "--is-ancestor", older, newer).returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


def saved_by_restore(path, since_ns):
    """Did infra/dev/restore-shared save `path`'s change during the call?
    It names each save `<time>-<pid>-<path, / as _>.patch` in the folder
    below, and writes it before it touches the file. Two seconds of slack
    for a filesystem whose mtime is coarser than the picture's clock."""
    save = os.environ.get("BOSS_RESTORE_SAVE") or os.path.join(
        os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state"), "boss", "restored")
    suffix = "-" + path.replace("/", "_") + ".patch"
    try:
        return any(e.name.endswith(suffix) and e.stat().st_mtime_ns >= since_ns - 2_000_000_000
                   for e in os.scandir(save))
    except OSError:
        return False


def after_call(payload, shared, worktree):
    try:
        path = picture_file(payload)
        with open(path) as f:
            then = json.load(f)
        os.unlink(path)
    except Exception:
        return 0
    now = picture(shared)
    if now is None or not isinstance(then, dict) or "sha" not in then:
        return 0
    moved = []
    if (now["head"], now["sha"]) != (then["head"], then["sha"]):
        # The one move that is not damage: main, at a commit origin/main
        # holds, reached FORWARD when it was main before — the hourly
        # fast-forward — or from wherever it was — the sidecar's return of
        # a clean checkout (88553fea). Anything else (a commit, a reset, a
        # merge, a checkout) moved the tree every door runs.
        repaired = (now["head"] == "refs/heads/main"
                    and is_ancestor(shared, now["sha"], "refs/remotes/origin/main")
                    and (then["head"] != now["head"] or is_ancestor(shared, then["sha"], now["sha"])))
        if repaired:
            pass
        elif now["head"] != then["head"]:
            moved.append(f"HEAD went from {then['head']} to {now['head']}")
        else:
            moved.append(f"{now['head']} went from {then['sha'][:8]} to {now['sha'][:8]}, which is not a "
                         f"fast-forward to a commit origin/main holds — a commit, reset or merge ran there "
                         f"(reflog: git -C {shared} reflog -3)")

    def listed(paths):
        more = f" and {len(paths) - 20} more" if len(paths) > 20 else ""
        return ", ".join(paths[:20]) + more

    written = sorted(p for p, v in now["paths"].items() if then["paths"].get(p) != v)
    if written:
        moved.append("written: " + listed(written))
    gone = sorted(p for p in then["paths"] if p not in now["paths"]
                  and not saved_by_restore(p, int(then.get("at") or 0)))
    if gone:
        moved.append("changes that were there before the call and are not now (discarded or "
                     "committed), which restore-shared did not save: " + listed(gone))
    if not moved:
        return 0
    sys.stderr.write(
        f"shared-checkout guard (backlog aebd7439): while this call ran, the shared checkout {shared} "
        f"changed — {'; '.join(moved)}. A call whose leading `cd` the harness drops runs THERE, not in "
        f"your worktree {worktree}; this call did it, or another session's call during it did. "
        f"Put each tracked file you wrote back with `{shared}/infra/dev/restore-shared <file>` (one file "
        f"per argument; it saves the discarded change first). A new untracked file, a discarded change, "
        f"a commit or a moved HEAD is not yours to undo from a worktree: stop and report it to your "
        f"coordinator, who reads what it carries and runs `git checkout main` in {shared}. Then redo the step "
        f"with absolute worktree paths, git -C {worktree}, or a script whose first line is "
        f"`cd {worktree} || exit 1`.\n")
    return 2


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
    if payload.get("hook_event_name") in ("PostToolUse", "PostToolUseFailure"):
        return after_call(payload, shared, worktree) if payload.get("tool_name") == "Bash" else 0

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
        # Text it cannot parse refuses nothing — and is exactly what the
        # after-call check is for.
        before_call(payload, shared)
        return 0
    # Judged from BOTH places the call may run — the cwd the payload
    # reports and the shared checkout — until a cd names one; see WHERE A
    # CALL RUNS. A cd with an absolute target collapses them to one.
    bases = [cwd, shared]
    for n, (sep, words) in enumerate(commands):
        judge_inline(words, command, bases, into_shared, ways)
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
    # Passed: picture <shared> for the after-call check (aebd7439).
    before_call(payload, shared)
    return 0


if __name__ == "__main__":
    sys.exit(main())
