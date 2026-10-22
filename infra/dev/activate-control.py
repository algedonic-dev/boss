#!/usr/bin/python3 -I
"""Activate an already verified published-image control artifact.

The image installer supplies the exact SHA generation's archive. This
door does not fetch an image, accept worker artifacts or create grants.
"""
import argparse
import base64
from contextlib import contextmanager
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import stat
import sys
import tempfile
import uuid


# THE DURABLE HOME (design a68fc42a D6, David 2026-10-10). The store and
# its one selector lived on the dev container's root filesystem
# (under /opt and /etc), so every pod
# roll erased them and all four doors silently went back to running
# candidate code beside the token (gap (a)). On the /work PVC they
# survive a roll, and the reclaim sidecar, which runs the hourly
# installer and cannot write the dev container's root fs, can keep
# them current. Workers never mount /work. The doors (infra/dev/boss,
# boss-api, wt-cargo, wt-web) and the CLI's child forwarding
# (door_env.rs) spell this path; control-tools_test.py holds them equal.
STORE = Path("/work/tools/dev-control")
SELECTOR = STORE / "enabled"
# Generations kept while following main: the active one, the one it
# replaced (a door may still be running from it), and three more. Each
# is ~95 MB, and main moves ~25 times a day on a 40Gi volume.
KEEP = 5


def published_module(name):
    path = Path(__file__).resolve().with_name(name + ".py")
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def atomic_file(path, raw):
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    if path.parent.is_symlink() or path.parent.stat().st_uid != os.getuid():
        raise ValueError("unsafe controller record parent")
    with tempfile.NamedTemporaryFile(prefix=".control-", dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)


def atomic_link(parent, name, target):
    temporary = parent / (".link-" + uuid.uuid4().hex)
    try:
        temporary.symlink_to(target)
        os.replace(temporary, parent / name)
    finally:
        temporary.unlink(missing_ok=True)


def run_contract(argv, env):
    return subprocess.run(argv, env=env, capture_output=True, timeout=30, check=False)


@contextmanager
def transition_lock(store):
    store.mkdir(mode=0o700, parents=True, exist_ok=True)
    if store.is_symlink() or store.stat().st_uid != os.getuid() or store.stat().st_mode & 0o077:
        raise ValueError("unsafe controller generation store")
    fd = os.open(store / ".activation.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "rb") as lock:
        info = os.fstat(lock.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise ValueError("unsafe controller activation lock")
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        yield


def deactivate(store, enabled):
    """Supported rollback: serialize with activation and installer refusal.

    A bare unlink takes no advisory lock and can be undone by a pass
    already running (review 2c4be21d, car 3f64de32). Keep the store and
    lock, record off before unlinking, then acknowledge only after both.
    The intent also stops a follow-main pass after an interrupted off.
    """
    with transition_lock(store):
        atomic_file(store / "off.json", json.dumps({"state": "off"}).encode())
        enabled.unlink(missing_ok=True)
        fd = os.open(store, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    return "off"


def activate(archive, store, enabled, commit, config, runner=run_contract, follow=False, prepared_config=None):
    with transition_lock(store):
        if follow:
            # The supported deactivate door takes this same lock; never
            # recommend bare rm as a rollback of an advisory-lock protocol.
            if (store / "off.json").exists() or (store / "off.json").is_symlink():
                record = published_module("dev-build").load_record(store / "off.json")
                if record != {"state": "off"}:
                    raise ValueError("invalid controller off intent")
                enabled.unlink(missing_ok=True)
                return "inert"
            if not (enabled.is_symlink() or enabled.exists()):
                return "inert"
            config = published_module("dev-build").load_record(prepared_config or archive.parent / "config.json")
        activate_locked(archive, store, enabled, commit, config, runner)
        # Only explicit activation may revoke an off intent. If this
        # unlink is interrupted the next follow-main pass stays off.
        if not follow:
            (store / "off.json").unlink(missing_ok=True)
        return "activated " + commit


def reactivate(archive, store, enabled, commit, runner=run_contract, prepared_config=None):
    """Follow main with this published generation's prepared configuration,
    ONLY where a selector already exists.

    No selector is inert, never an activation: turning isolation on is
    the operator's act (b3071bf4, held_by_david_20261009). A refused
    selector is re-tried, so a bad hour heals on the next good one. A
    failure raises and changes no authority; the installer then refuses
    the selector (design a68fc42a D1), because a selector it could not
    re-establish must not leave candidate code running beside the token.
    Missing or unsafe generation configuration refuses; the previous
    generation's binding never substitutes for unprepared published data.
    """
    return activate(archive, store, enabled, commit, None, runner, follow=True, prepared_config=prepared_config)


def prune(store, keep, active):
    generations = store / "generations"
    found = [p for p in generations.iterdir() if re.fullmatch(r"[0-9a-f]{40}", p.name) and p.is_dir() and not p.is_symlink()]
    found.sort(key=lambda p: p.stat().st_mtime, reverse=True)
    for old in found[keep:]:
        if old.name not in active:
            shutil.rmtree(old)


def activate_locked(archive, store, enabled, commit, config, runner):
    store.mkdir(mode=0o700, parents=True, exist_ok=True)
    if store.is_symlink() or store.stat().st_uid != os.getuid() or store.stat().st_mode & 0o077:
        raise ValueError("unsafe controller generation store")
    if set(config) != {"worker_image", "postgres_image", "forge_url"}:
        raise ValueError("controller configuration contains undeclared fields")
    worker = published_module("dev-build")
    worker.worker_job("dev-build-0000000000000000", "0" * 40, config["worker_image"], config["forge_url"], config["postgres_image"])
    generation = store / "generations" / commit
    artifact = published_module("control-artifact")
    if generation.exists():
        artifact.validate_generation(generation, commit)
    else:
        artifact.extract(archive, generation, commit)
    # Only the image-owned binary runs, under a fixed environment.
    # A version string from a candidate target is never consulted.
    receipt = {"id": uuid.uuid4().hex, "commit": commit, "completion": "unproven"}
    try:
        result = runner([str(generation / "bin/boss"), "dev-control-contract"],
                        {"HOME": "/work/home", "PATH": "/usr/bin:/bin", "BOSS_BUILD_COMMIT": commit})
        receipt.update(exit=result.returncode, completion="observed",
                       stdout_base64=base64.b64encode(result.stdout).decode("ascii"),
                       stderr_base64=base64.b64encode(result.stderr).decode("ascii"))
    except subprocess.TimeoutExpired as error:
        receipt.update(error="contract check timed out; activation unproven",
                       stdout_base64=base64.b64encode(error.stdout or b"").decode("ascii"),
                       stderr_base64=base64.b64encode(error.stderr or b"").decode("ascii"))
        raise
    finally:
        worker.save_record(generation / ("contract-check-" + receipt["id"] + ".json"), receipt)
        # Latest is a machine copy; every original complete record is
        # retained separately, including interrupted/repeated checks.
        atomic_file(generation / "contract-check.json", json.dumps(receipt).encode())
    expected_contract = Path(__file__).resolve().with_name("control-contract.txt").read_bytes().strip()
    if result.returncode or result.stdout.strip() != expected_contract:
        raise ValueError("published CLI does not support the dev routing contract; existing mode preserved")
    config_path = generation / "config.json"
    if config_path.exists() or config_path.is_symlink():
        if worker.load_record(config_path) != config:
            raise ValueError("published generation configuration is immutable")
    else:
        atomic_file(config_path, json.dumps(config).encode())
    previous = os.readlink(store / "current") if (store / "current").is_symlink() else None
    if (store / "current").exists() and previous is None:
        raise ValueError("controller current path is not an owned generation link")
    atomic_file(store / "activation-intent.json", json.dumps({"commit": commit, "previous": previous}).encode())
    for tool in ("boss", "boss-api", "wt-cargo", "wt-web", "dev-build"):
        frontend = store / tool
        if frontend.exists() and not frontend.is_symlink():
            raise ValueError("controller frontend is not an owned generation link")
        atomic_link(store, tool, str(enabled / "infra/dev/control-tools.py"))
    enabled.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    if enabled.parent.is_symlink() or enabled.parent.stat().st_uid != os.getuid() or enabled.parent.stat().st_mode & 0o022:
        raise ValueError("unsafe active-generation parent")
    # All frontends resolve through this ONE pointer. Interruption
    # before its replace keeps the old authority; after it, every door
    # selects the complete new generation. No marker/current pair can
    # disagree, and an active generation's config is never rewritten.
    atomic_link(enabled.parent, enabled.name, str(generation))
    # Convenience observations are not routing authority. A crash can
    # omit these records without leaving split active generations.
    atomic_link(store, "current", "generations/" + commit)
    atomic_file(store / "activation-observed.json", json.dumps({"commit": commit, "current": os.readlink(store / "current"), "enabled": os.readlink(enabled)}).encode())
    # Only after the new authority and its observation are complete.
    prune(store, KEEP, {commit})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("commit")
    parser.add_argument("--cli-store", type=Path, default=Path("/work/tools/image-cli"))
    parser.add_argument("--store", type=Path, default=STORE)
    parser.add_argument("--prepared-config", type=Path)
    parser.add_argument("--main", help="the commit the image store names current (an explicit activation's receipt is judged against it)")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--config", type=Path)
    mode.add_argument("--reactivate", action="store_true",
                      help="follow main: re-activate through an EXISTING selector only (inert without one)")
    mode.add_argument("--deactivate", action="store_true", help="supported rollback under the activation lock")
    args = parser.parse_args()
    archive = args.cli_store / args.commit / "dev-control.tar"
    enabled = args.store / SELECTOR.name
    if args.deactivate:
        outcome = deactivate(args.store, enabled)
    elif args.reactivate:
        outcome = reactivate(archive, args.store, enabled, args.commit, prepared_config=args.prepared_config)
    else:
        # Backlog b50b8953 (David, 2026-10-19): both flips that day failed
        # first at a step a rehearsal reads — a Longhorn floor, a binding
        # a nested worktree could not select. The operator's activation
        # refuses, before it extracts or links anything, unless THIS
        # commit's preflight receipt passed every check at the main the
        # image store names now. Follow-main (--reactivate) needs none: it
        # only keeps an activation already made current.
        published_module("preflight-control").require_preflight(args.store, args.commit, args.main)
        config = published_module("dev-build").load_record(args.config)
        outcome = activate(archive, args.store, enabled, args.commit, config)
    if outcome == "off":
        print("dev-control: off — supported rollback recorded at " + str(args.store / "off.json"))
    elif outcome == "inert":
        print("dev-control: inert — no selector at " + str(enabled) + "; nothing was activated (activating is the operator's act)")
    else:
        print("dev-control: " + outcome + " — " + str(enabled) + " -> " + os.readlink(enabled))


if __name__ == "__main__":
    main()
