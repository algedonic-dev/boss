#!/usr/bin/python3 -I
"""Activate an already verified published-image control artifact.

The image installer supplies the exact SHA generation's archive. This
door does not fetch an image, accept worker artifacts or create grants.
"""
import argparse
import base64
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import stat
import sys
import tempfile
import uuid


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


def activate(archive, store, enabled, commit, config, runner=run_contract):
    store.mkdir(mode=0o700, parents=True, exist_ok=True)
    if store.is_symlink() or store.stat().st_uid != os.getuid() or store.stat().st_mode & 0o077:
        raise ValueError("unsafe controller generation store")
    fd = os.open(store / ".activation.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "rb") as lock:
        info = os.fstat(lock.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise ValueError("unsafe controller activation lock")
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        return activate_locked(archive, store, enabled, commit, config, runner)


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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("commit")
    parser.add_argument("--cli-store", type=Path, default=Path("/work/tools/image-cli"))
    parser.add_argument("--config", type=Path, required=True)
    args = parser.parse_args()
    config = published_module("dev-build").load_record(args.config)
    activate(args.cli_store / args.commit / "dev-control.tar", Path("/opt/boss-dev-control"),
             Path("/etc/boss-dev-control/enabled"), args.commit, config)


if __name__ == "__main__":
    main()
