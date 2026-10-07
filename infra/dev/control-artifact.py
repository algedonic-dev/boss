#!/usr/bin/python3 -I
"""Pack image-owned control files; validate all bytes before extraction.

The existing verified-image installer supplies this artifact. Its
internal manifest conserves bytes and paths, not image authenticity.
The installer also publishes this file as image-cli/<commit>/dev-control.
Explicit activation uses that installed door with `activate <full commit>
--config <reviewed configuration>`; installation never invokes it.
"""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import tarfile
import tempfile


MAX_BYTES = 512 * 1024 * 1024


def safe_name(name):
    path = PurePosixPath(name)
    if not name or path.is_absolute() or any(p in ("", ".", "..") for p in name.split("/")) or "\\" in name:
        raise ValueError("unsafe artifact member path")
    return path


def pack(source, binaries, destination, commit):
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("artifact requires the full published commit")
    files = {}
    for line in (source / "infra/dev/control-files.txt").read_text().splitlines():
        if not line or line.startswith("#"):
            continue
        safe_name(line)
        files[line] = source / line
    for name in ("boss", "kubectl"):
        files["bin/" + name] = binaries / name
    payload = {}
    for name, path in files.items():
        if path.is_symlink() or not path.is_file():
            raise ValueError("control artifact requires regular source files: " + name)
        raw = path.read_bytes()
        mode = 0o755 if path.stat().st_mode & 0o111 or name.startswith("bin/") else 0o644
        payload[name] = (raw, mode)
    if sum(len(raw) for raw, _ in payload.values()) > MAX_BYTES:
        raise ValueError("artifact exceeds bounded size")
    record = {"commit": commit, "files": {n: {"sha256": hashlib.sha256(raw).hexdigest(), "mode": mode} for n, (raw, mode) in payload.items()}}
    payload["artifact.json"] = (json.dumps(record, sort_keys=True).encode(), 0o644)
    with tarfile.open(destination, "w") as archive:
        for name, (raw, mode) in sorted(payload.items()):
            member = tarfile.TarInfo(name)
            member.size = len(raw)
            member.mode = mode
            archive.addfile(member, io.BytesIO(raw))


def extract(archive_path, destination, commit):
    if not re.fullmatch(r"[0-9a-f]{40}", commit) or destination.exists() or destination.is_symlink():
        raise ValueError("unsafe or existing control generation")
    payload = {}
    modes = {}
    total = 0
    with tarfile.open(archive_path, "r:") as archive:
        for member in archive:
            safe_name(member.name)
            if not member.isfile() or member.name in payload or member.mode & 0o7000:
                raise ValueError("artifact contains a link, special file or duplicate member")
            total += member.size
            if total > MAX_BYTES or member.size < 0 or len(payload) >= 10000:
                raise ValueError("artifact exceeds bounded size")
            stream = archive.extractfile(member)
            if stream is None:
                raise ValueError("artifact member has no bytes")
            payload[member.name] = stream.read(member.size + 1)
            modes[member.name] = member.mode
            if len(payload[member.name]) != member.size:
                raise ValueError("artifact member size changed")
    metadata = payload.pop("artifact.json", None)
    if metadata is None or len(metadata) > 1024 * 1024:
        raise ValueError("artifact manifest is missing or oversized")
    record = json.loads(metadata)
    if record["commit"] != commit or set(record["files"]) != set(payload):
        raise ValueError("artifact commit or declared members differ")
    for name, raw in payload.items():
        expected = record["files"][name]
        if expected["mode"] not in (0o644, 0o755) or modes[name] != expected["mode"] or hashlib.sha256(raw).hexdigest() != expected["sha256"]:
            raise ValueError("artifact member mode or digest differs")
    # Validation completes before any destination exists. The temporary
    # directory is private and contains no links; tar never extracts.
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".control-", dir=destination.parent) as temporary:
        staged = Path(temporary) / "generation"
        staged.mkdir(mode=0o700)
        for name, raw in payload.items():
            target = staged / name
            target.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
            fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, record["files"][name]["mode"])
            os.fchmod(fd, record["files"][name]["mode"])
            with os.fdopen(fd, "wb") as stream:
                stream.write(raw)
        (staged / "artifact.json").write_bytes(metadata)
        os.rename(staged, destination)


def validate_generation(destination, commit):
    if destination.is_symlink() or destination.stat().st_uid != os.getuid() or destination.stat().st_mode & 0o077:
        raise ValueError("unsafe controller generation")
    fd = os.open(destination / "artifact.json", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022 or info.st_size > 1024 * 1024:
            raise ValueError("controller manifest is not owned bounded regular data")
        raw = stream.read(1024 * 1024 + 1)
        if len(raw) > 1024 * 1024:
            raise ValueError("controller manifest exceeds its bound")
        record = json.loads(raw)
    if record["commit"] != commit:
        raise ValueError("controller generation commit changed")
    for name, expected in record["files"].items():
        safe_name(name)
        path = destination / name
        if any(parent.is_symlink() for parent in path.parents if parent != destination.parent):
            raise ValueError("controller generation contains a linked parent")
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, "rb") as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != expected["mode"]:
                raise ValueError("controller generation ownership or mode changed")
            if hashlib.sha256(stream.read(MAX_BYTES + 1)).hexdigest() != expected["sha256"]:
                raise ValueError("controller generation bytes changed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="verb", required=True)
    build = sub.add_parser("pack")
    for name in ("source", "binaries", "destination"):
        build.add_argument(name, type=Path)
    build.add_argument("commit")
    unpack = sub.add_parser("extract")
    unpack.add_argument("archive", type=Path)
    unpack.add_argument("destination", type=Path)
    unpack.add_argument("commit")
    activation = sub.add_parser("activate", help="explicitly activate a verified installed image artifact")
    activation.add_argument("commit")
    activation.add_argument("--cli-store", type=Path, default=Path("/work/tools/image-cli"))
    activation.add_argument("--config", type=Path, required=True)
    args = parser.parse_args()
    if args.verb == "pack":
        pack(args.source, args.binaries, args.destination, args.commit)
    elif args.verb == "extract":
        extract(args.archive, args.destination, args.commit)
    else:
        bootstrap(args.cli_store, args.commit, args.config)


def bootstrap(store, commit, config):
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("activation requires the full installed commit")
    generation = store / commit
    if generation.is_symlink() or not generation.is_dir():
        raise ValueError("activation requires an installed regular generation")
    fd = os.open(generation / "dev-control.tar", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022 or info.st_size > MAX_BYTES:
            raise ValueError("activation archive is not owned bounded regular data")
        # Capture one archive before validating. Execution reads only the
        # private copy, never mutable checkout or candidate target bytes.
        raw = source.read(MAX_BYTES + 1)
        if len(raw) != info.st_size or len(raw) > MAX_BYTES:
            raise ValueError("activation archive changed or exceeded its bound")
    with tempfile.TemporaryDirectory(prefix="boss-control-bootstrap-") as temporary:
        root = Path(temporary)
        archive = root / "published.tar"
        archive.write_bytes(raw)
        staged = root / commit
        extract(archive, staged, commit)
        if (staged / "infra/dev/control-artifact.py").read_bytes() != Path(__file__).read_bytes():
            raise ValueError("installed bootstrap differs from its published archive generation")
        if not (staged / "infra/dev/activate-control.py").is_file():
            raise ValueError("activation entrypoint is absent from published closure")
        (staged / "dev-control.tar").write_bytes(raw)
        result = subprocess.run(["/usr/bin/python3", "-I", str(staged / "infra/dev/activate-control.py"), commit,
                                 "--cli-store", str(root), "--config", str(config.resolve())],
                                env={"PATH": "/usr/bin:/bin", "HOME": "/work/home"}, check=False)
        if result.returncode:
            raise ValueError("published activation refused with exit " + str(result.returncode))


if __name__ == "__main__":
    main()
