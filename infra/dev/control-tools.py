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


def tool_plan(generation, tool, args, inherited):
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
    for key in ("BOSS_ACTOR", "BOSS_JOBS_URL", "BOSS_SOR_SERVICE", "BOSS_SOR_WAIT_SECONDS"):
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
    argv, env = tool_plan(generation, invoked, args, os.environ)
    os.execve(argv[0], argv, env)


if __name__ == "__main__":
    main()
