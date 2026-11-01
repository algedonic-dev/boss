#!/usr/bin/python3 -I
"""Published dev doors: trusted API/CLI here, candidate programs there."""
import json
import hashlib
import os
from pathlib import Path
import sys
import re
import stat
import types
from urllib.parse import urlsplit


def observed_generation(generation):
    # A published frontend observes its immutable generation, not a
    # mutable checkout's freshness helper or candidate Python module.
    info = generation.stat()
    if generation.is_symlink() or info.st_uid != os.getuid() or info.st_mode & 0o077 or not re.fullmatch(r'[0-9a-f]{40}', generation.name):
        raise ValueError('published frontend has no owned generation authority')
    fd = os.open(generation / 'artifact.json', os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022 or info.st_size > 1024 * 1024:
            raise ValueError('published generation manifest is unsafe')
        record = json.loads(stream.read(1024 * 1024 + 1))
    if record['commit'] != generation.name:
        raise ValueError('published generation commit binding changed')
    helper = generation / 'infra/dev/control-artifact.py'
    if any(parent.is_symlink() for parent in helper.parents if parent != generation.parent):
        raise ValueError('published validation helper has a linked parent')
    fd = os.open(helper, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        expected = record['files']['infra/dev/control-artifact.py']
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != expected['mode'] or info.st_size > 1024 * 1024:
            raise ValueError('published validation helper is unsafe')
        raw = stream.read(1024 * 1024 + 1)
        if hashlib.sha256(raw).hexdigest() != expected['sha256']:
            raise ValueError('published validation helper bytes changed')
    # Execute exactly the verified bytes, never a loader's second read
    # or timestamp-valid pycache file outside the admitted manifest.
    module = types.ModuleType('published_control_artifact')
    module.__file__ = str(helper)
    exec(compile(raw, str(helper), 'exec'), module.__dict__)
    module.validate_generation(generation, generation.name)
    return generation.name


# What the trusted session doors (boss, boss-api) take from the caller:
# who signs and which agent run the verb is — the CLI's IDENTITY_ENV,
# all three — then where the call goes. Backlog 19dbaab9 (2026-10-18,
# first hour of isolation ON): this carried BOSS_ACTOR alone, so
# BOSS_AGENT_RUN never arrived (no run could record --started, no
# gate-run named the run that launched it) and BOSS_ACTOR_FILE fell
# back to the shared $HOME/.config/boss/actor, another session's id.
# The pin `the_published_door_carries_every_identity_name_the_cli_reads`
# (boss-cli door_env.rs) holds this to IDENTITY_ENV.
#
# Then two names that are not identity but that the session's own verbs
# read (backlog c02f30c5, found 2026-10-18 building 19dbaab9):
# BOSS_SESSION_AGENT_DEFINITIONS, the snapshot behind dispatch's
# unloaded-definitions refusal, without which that refusal passed every
# call in silence; and BOSS_COMMIT_TRAILER, the attribution lines every
# rendered brief carries. The same pin holds them to the CLI's
# SESSION_DATA_ENV (door_env.rs). Neither reaches a worker plan.
SESSION_ENV = (
    "BOSS_ACTOR",
    "BOSS_ACTOR_FILE",
    "BOSS_AGENT_RUN",
    "BOSS_COMMIT_TRAILER",
    "BOSS_JOBS_URL",
    "BOSS_SESSION_AGENT_DEFINITIONS",
    "BOSS_SOR_SERVICE",
    "BOSS_SOR_WAIT_SECONDS",
)


def shared_checkout_refusal(tool, inherited, cwd=None):
    """wt-cargo's own shared-checkout guard (backlog 916fe290), at this door.

    Backlog c02f30c5: with isolation on, wt-cargo forwards here BEFORE its
    guard runs, the worker plan carries no BOSS_AGENT_RUN, and the
    worker's source is not the shared shape — so the guard never fires.
    The controller's cwd binding does not cover it: the shared checkout is
    itself bound (the doors run from it, 7dd185c6), so a builder whose cd
    never took builds that tree in a worker, silently. Shared is judged by
    the shape wt-cargo judges with git rev-parse — a `.git` DIRECTORY (git
    dir equal to common dir) with worktrees registered under it — read off
    the filesystem, since this door runs no git. Returns the refusal, or
    None when wt-cargo held would build.

    Backlog 2e5429b6: the cwd is read HERE, and only for wt-cargo naming a
    run — main() used to read it for every tool first, so a session whose
    worktree was removed lost every boss and boss-api call to a
    FileNotFoundError traceback. What cannot be read is a named refusal.
    """
    run = inherited.get("BOSS_AGENT_RUN", "")
    if tool != "wt-cargo" or not run.strip():
        return None
    try:
        here = Path(os.getcwd() if cwd is None else cwd)
    except OSError as gone:
        return (
            "wt-cargo: REFUSED — BOSS_AGENT_RUN=" + run + " names an agent run, and this shell's "
            "current directory no longer exists (" + str(gone) + "): the worktree it stood in was "
            "removed, so there is no tree to build (backlog 2e5429b6). cd into the run's own "
            "worktree from a script, then run wt-cargo again."
        )
    for tree in (here, *here.parents):
        dotgit = tree / ".git"
        if not os.path.lexists(dotgit):
            continue
        worktrees = dotgit / "worktrees"
        try:
            if not dotgit.is_dir() or not worktrees.is_dir() or not any(worktrees.iterdir()):
                return None
        except OSError as unreadable:
            return (
                "wt-cargo: REFUSED — BOSS_AGENT_RUN=" + run + " names an agent run, and "
                + str(worktrees) + " cannot be read (" + str(unreadable) + "), so this door cannot "
                "tell whether " + str(tree) + " is the SHARED checkout (backlog 2e5429b6). Run "
                "wt-cargo from the run's own worktree."
            )
        return (
            "wt-cargo: REFUSED — BOSS_AGENT_RUN=" + run + " names an agent run, and this shell is in "
            + str(tree) + ", the SHARED checkout: the main checkout worktrees hang off and every door "
            "on this host runs from, not a worktree of the run's own (backlog 916fe290; held at the "
            "isolation door since c02f30c5, because the worker never sees the run). Run wt-cargo from "
            "a script whose first lines are `cd <your worktree> || exit 1` and `pwd`. An operator "
            "building this checkout on purpose unsets BOSS_AGENT_RUN."
        )
    return None


def tool_plan(generation, tool, args, inherited):
    # A recorded probe's own writer supplies this channel even when it
    # cannot open a log. Refuse before clearing inherited markers or
    # selecting the credential-bearing session door (ca356c79).
    if tool in ("boss", "boss-api") and (
        "BOSS_PROBE_NOTFOUND" in inherited
        or inherited.get("BOSS_SOR_USER")
        or inherited.get("BOSS_SOR_DOOR")
    ):
        raise ValueError("recorded probe context must read through boss-sor-read, never this session door")
    # No loader hooks, provider tokens, mutable executable directories,
    # npm settings, git settings or worker environment are inherited.
    env = {"HOME": "/work/home", "PATH": "/usr/bin:/bin"}
    controller = ["/usr/bin/python3", "-I", str(generation / "infra/dev/dev-build.py")]
    for key in ("KUBERNETES_SERVICE_HOST", "KUBERNETES_SERVICE_PORT", "KUBERNETES_SERVICE_PORT_HTTPS"):
        if key in inherited:
            env[key] = inherited[key]
    if tool in ("wt-cargo", "wt-web"):
        return [*controller, "exec-auto", "--", "/workspace/source/infra/dev/" + tool, *args], env
    if tool == "dev-build":
        return [*controller, *args], env
    if tool == "boss" and (args[:1] == ["--built"] or inherited.get("BOSS_SHIM_BUILT")):
        candidate_args = args[1:] if args[:1] == ["--built"] else args
        return [*controller, "exec-auto", "--", "/workspace/source/infra/dev/wt-cargo", "run", "-p", "boss-cli", "--bin", "boss", "--", *candidate_args], env
    if tool not in ("boss", "boss-api"):
        raise ValueError("unknown published control door")
    # These are the existing trusted actor's data arguments, not a
    # worker stamping service. The worker mounts none of these doors.
    for key in SESSION_ENV:
        if key in inherited:
            env[key] = inherited[key]
    if tool == "boss-api":
        return ["/bin/bash", str(generation / "infra/dev/boss-api"), *args], env
    record = json.loads((generation / "artifact.json").read_text())
    env["BOSS_BUILD_COMMIT"] = record["commit"]
    data = generation / "infra/dev"
    env["BOSS_MACHINE_TOKEN_DIR"] = (data / "machine-token-dir").read_text().strip()
    if not env["BOSS_MACHINE_TOKEN_DIR"].startswith("/"):
        raise ValueError("published machine gate path is missing")
    hosts = [urlsplit((data / name).read_text().strip()).hostname for name in ("sor-url", "sor-read-url")]
    if not all(hosts):
        raise ValueError("published system-of-record hosts are missing")
    env["BOSS_MACHINE_TOKEN_HOSTS"] = ",".join(hosts)
    env.setdefault("BOSS_JOBS_URL", (data / "sor-url").read_text().strip())
    # Backlog 63d59f45: the CLI runs `kubectl` (gate launch, withdraw,
    # gate-Job reads), and the pod's own copy is in the group-writable
    # /work/tools/bin. It runs THIS generation's bin/kubectl — owned,
    # hash-checked by observed_generation on every call, the same one
    # dev-build.py runs — named by absolute path, as bin/boss is above.
    # Backlog 7ed1251a (RELEASE review f6e0c3e5): this used to put bin/
    # first on PATH, but the listing of bin/ is checked once, here, and
    # the CLI looks names up for as long as it runs, so a name planted
    # in bin/ after the check (a `git`) would resolve there. Nothing of
    # the generation is on PATH now, so no lookup can land in bin/. The
    # name is the CLI's DOOR_KUBECTL_ENV (door_env.rs), and the pin
    # `the_published_door_hands_the_cli_the_kubectl_it_checked` holds
    # the two spellings equal.
    env["BOSS_DOOR_KUBECTL"] = str(generation / "bin/kubectl")
    return [str(generation / "bin/boss"), *args], env


def main():
    generation = Path(__file__).resolve().parents[2]
    observed_generation(generation)
    invoked = Path(sys.argv[0]).name
    if invoked == "control-tools.py":
        if len(sys.argv) < 2:
            raise ValueError("published control door needs a name")
        invoked, args = sys.argv[1], sys.argv[2:]
    else:
        args = sys.argv[1:]
    refused = shared_checkout_refusal(invoked, os.environ)
    if refused is not None:
        print(refused, file=sys.stderr)
        sys.exit(2)
    argv, env = tool_plan(generation, invoked, args, os.environ)
    os.execve(argv[0], argv, env)


if __name__ == "__main__":
    main()
