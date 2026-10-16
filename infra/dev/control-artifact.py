#!/usr/bin/python3 -I
"""Pack image-owned control files; validate all bytes before extraction.

The existing verified-image installer supplies this artifact. Its
internal manifest conserves bytes and paths, not image authenticity.
The installer also publishes this file as image-cli/<commit>/dev-control.
Explicit activation uses that installed door with `activate <full commit>
--config <reviewed configuration>`. Installation never ACTIVATES: where a
selector already exists the installer runs `activate <full commit>
--reactivate` so the doors follow main (design a68fc42a D6), and with no
selector that pass is inert.

Rollback uses the SAME transition lock as follow-main and refusal:
image-cli/<verified-sha>/dev-control deactivate <verified-sha>.
It validates the published archive, records off durably, and removes
the selector under the lock. Keep the store and its lock; bare rm of
enabled or the store is not a serialized rollback. After this door
acknowledges off, maintenance cannot turn isolation back on. Only a
new explicit activate with reviewed config clears off. All live
activation/deactivation remains a separately authorized operator act.
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
import tomllib
from urllib.parse import urlsplit


MAX_BYTES = 512 * 1024 * 1024
CONFIG_INPUT_NAMES = ("infra/dev/control-images.json", "infra/estate/estate.toml")


def copy_bounded(source, destination, maximum):
    """Hash every byte while retaining at most one small transfer buffer."""
    hasher = hashlib.sha256()
    total = 0
    while chunk := source.read(min(65536, maximum - total + 1)):
        total += len(chunk)
        if total > maximum:
            raise ValueError("artifact exceeds bounded size")
        hasher.update(chunk)
        if destination is not None:
            destination.write(chunk)
    return total, hasher.hexdigest()


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


def extract(archive_path, destination, commit, retain=None):
    if not re.fullmatch(r"[0-9a-f]{40}", commit) or destination.exists() or destination.is_symlink():
        raise ValueError("unsafe or existing control generation")
    # f1a22bd6: the reclaim reader has 256 MiB, while the published tools
    # alone occupy ~97 MB. Hash all members, without retaining the archive
    # plus every executable in RAM just to read two configuration inputs.
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".control-", dir=destination.parent) as temporary:
        staged = Path(temporary) / "generation"
        staged.mkdir(mode=0o700)
        observed = {}
        directories = set()
        retained = []
        total = 0
        with tarfile.open(archive_path, "r:") as archive:
            for member in archive:
                path = safe_name(member.name)
                if not member.isfile() or member.name in observed or member.mode & 0o7000:
                    raise ValueError("artifact contains a link, special file or duplicate member")
                parents = {str(parent) for parent in path.parents if str(parent) != "."}
                if member.name in directories or parents.intersection(observed):
                    raise ValueError("artifact file paths conflict")
                directories.update(parents)
                total += member.size
                if total > MAX_BYTES or member.size < 0 or len(observed) >= 10000:
                    raise ValueError("artifact exceeds bounded size")
                if member.name == "artifact.json" and member.size > 1024 * 1024:
                    raise ValueError("artifact manifest is missing or oversized")
                stream = archive.extractfile(member)
                if stream is None:
                    raise ValueError("artifact member has no bytes")
                if retain is None or member.name == "artifact.json" or member.name in retain:
                    target = staged / member.name
                    target.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
                    fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
                    with os.fdopen(fd, "wb") as output:
                        length, digest = copy_bounded(stream, output, member.size)
                    retained.append(member.name)
                else:
                    length, digest = copy_bounded(stream, None, member.size)
                if length != member.size:
                    raise ValueError("artifact member size changed")
                observed[member.name] = {"sha256": digest, "mode": member.mode}
        if "artifact.json" not in observed:
            raise ValueError("artifact manifest is missing or oversized")
        record = json.loads((staged / "artifact.json").read_bytes())
        observed.pop("artifact.json")
        if record["commit"] != commit or set(record["files"]) != set(observed):
            raise ValueError("artifact commit or declared members differ")
        for name, actual in observed.items():
            expected = record["files"][name]
            if expected["mode"] not in (0o644, 0o755) or actual != expected:
                raise ValueError("artifact member mode or digest differs")
        # Only a fully validated private staging tree becomes a generation.
        # No executable is invoked, and no public destination exists earlier.
        for name in retained:
            os.chmod(staged / name, 0o644 if name == "artifact.json" else record["files"][name]["mode"])
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
            if copy_bounded(stream, None, MAX_BYTES)[1] != expected["sha256"]:
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
    activation.add_argument("--store", type=Path, help="the controller store (default: activate-control.py's own)")
    # Exactly one: an operator's activation names its reviewed config;
    # the hourly installer's follow-main pass (design a68fc42a D6) names
    # none and re-activates only through a selector that already exists.
    mode = activation.add_mutually_exclusive_group(required=True)
    mode.add_argument("--config", type=Path)
    mode.add_argument("--reactivate", action="store_true")
    deactivation = sub.add_parser("deactivate", help="supported rollback: serialize with reactivation and keep isolation off")
    deactivation.add_argument("commit")
    deactivation.add_argument("--cli-store", type=Path, default=Path("/work/tools/image-cli"))
    deactivation.add_argument("--store", type=Path)
    inputs = sub.add_parser("config-inputs", help="read validated generation-owned image and estate data; never activate")
    inputs.add_argument("commit")
    inputs.add_argument("--cli-store", type=Path, default=Path("/work/tools/image-cli"))
    args = parser.parse_args()
    if args.verb == "pack":
        pack(args.source, args.binaries, args.destination, args.commit)
    elif args.verb == "extract":
        extract(args.archive, args.destination, args.commit)
    elif args.verb == "config-inputs":
        bootstrap(args.cli_store, args.commit, None, read_inputs=True)
    else:
        bootstrap(args.cli_store, args.commit, getattr(args, "config", None), args.store,
                  deactivate=args.verb == "deactivate")


def config_inputs(generation, commit):
    names = CONFIG_INPUT_NAMES
    raw = {}
    for name in names:
        with (generation / name).open("rb") as source:
            raw[name] = source.read(65536 + 1)
    if any(len(value) > 65536 for value in raw.values()):
        raise ValueError("controller configuration inputs exceed their bound")
    images = json.loads(raw[names[0]])
    estate = tomllib.loads(raw[names[1]].decode())
    if set(images) != {"worker_image", "postgres_image", "forge_repository"}:
        raise ValueError("controller image inputs contain undeclared fields")
    registry = estate["forge_registry"]
    if not isinstance(registry, str) or not re.fullmatch(r"[A-Za-z0-9.:-]+", registry):
        raise ValueError("controller registry is not the estate's uncredentialed host")
    result = {"commit": commit}
    for field in ("worker_image", "postgres_image"):
        image = images[field]
        if set(image) != {"repository", "tag"} or not re.fullmatch(r"[a-z0-9_-]+(?:/[a-z0-9_-]+)+", image["repository"]) or not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}", image["tag"]):
            raise ValueError("controller image repository or tag is invalid")
        result[field] = registry + "/" + image["repository"] + ":" + image["tag"]
    repository = images["forge_repository"]
    url = estate["forge_url"]
    parsed = urlsplit(url)
    if not re.fullmatch(r"[a-z0-9_-]+(?:/[a-z0-9_-]+)+", repository) or parsed.scheme not in ("http", "https") or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment or parsed.path not in ("", "/"):
        raise ValueError("controller forge input is not an uncredentialed estate URL")
    result["forge_url"] = url.rstrip("/") + "/" + repository + ".git"
    result["source_sha256"] = {name: hashlib.sha256(value).hexdigest() for name, value in raw.items()}
    return result


def bootstrap(store, commit, config, control_store=None, deactivate=False, read_inputs=False):
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("activation requires the full installed commit")
    generation = store / commit
    if generation.is_symlink() or not generation.is_dir():
        raise ValueError("activation requires an installed regular generation")
    with tempfile.TemporaryDirectory(prefix="boss-control-bootstrap-") as temporary:
        root = Path(temporary)
        archive = root / "published.tar"
        fd = os.open(generation / "dev-control.tar", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, "rb") as source:
            info = os.fstat(source.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022 or info.st_size > MAX_BYTES:
                raise ValueError("activation archive is not owned bounded regular data")
            # Capture the same immutable private snapshot, in bounded chunks.
            # Neither candidate paths nor the original archive are executed.
            with archive.open("xb") as output:
                length, _ = copy_bounded(source, output, MAX_BYTES)
            if length != info.st_size:
                raise ValueError("activation archive changed or exceeded its bound")
        staged = root / commit
        retain = {*CONFIG_INPUT_NAMES, "infra/dev/control-artifact.py"} if read_inputs else None
        extract(archive, staged, commit, retain)
        if (staged / "infra/dev/control-artifact.py").read_bytes() != Path(__file__).read_bytes():
            raise ValueError("installed bootstrap differs from its published archive generation")
        if read_inputs:
            print(json.dumps(config_inputs(staged, commit), sort_keys=True))
            return
        if not (staged / "infra/dev/activate-control.py").is_file():
            raise ValueError("activation entrypoint is absent from published closure")
        os.rename(archive, staged / "dev-control.tar")
        argv = ["/usr/bin/python3", "-I", str(staged / "infra/dev/activate-control.py"), commit, "--cli-store", str(root)]
        if control_store is not None:
            argv += ["--store", str(control_store.resolve())]
        if deactivate:
            argv += ["--deactivate"]
        else:
            argv += ["--reactivate"] if config is None else ["--config", str(config.resolve())]
            if config is None:
                argv += ["--prepared-config", str(generation / "config.json")]
        result = subprocess.run(argv, env={"PATH": "/usr/bin:/bin", "HOME": "/work/home"}, check=False)
        if result.returncode:
            raise ValueError("published activation refused with exit " + str(result.returncode))


if __name__ == "__main__":
    main()
