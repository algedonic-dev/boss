#!/usr/bin/env python3
"""Publish fixed observation originals through the verified native file door."""
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import uuid

MAX_REPORT_BYTES = 1048576
ATTACHMENT_SECONDS = 45


def retain(directory, request, query_exit):
    if str(uuid.UUID(request)) != request:
        raise ValueError("invalid ops request identity")
    root = Path(directory)
    info = root.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ValueError("original directory is not private and owned")
    with (root / "query.exit").open("x") as target:
        target.write(str(query_exit) + "\n")
        target.flush()
        os.fsync(target.fileno())
    streams = {}
    for name in ("stdout", "stderr"):
        path = root / name
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1:
            raise ValueError("original stream is not an owned regular file")
        with path.open("rb") as source:
            os.fsync(source.fileno())
            if info.st_size > MAX_REPORT_BYTES:
                raise ValueError(name + " exceeds original attachment bound")
            raw = source.read(MAX_REPORT_BYTES + 1)
        if len(raw) != info.st_size:
            raise ValueError("original stream changed")
        streams[name] = {"filename": name, "bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}
    manifest = {"request_id": request, "original_query_exit": query_exit, **streams}
    path = root / "manifest.json"
    with path.open("x") as target:
        json.dump(manifest, target, sort_keys=True)
        target.write("\n")
        target.flush()
        os.fsync(target.fileno())
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)
    for name in ("stdout", "stderr", "manifest.json"):
        # Exit zero includes the file door's downloaded-byte equality check.
        with (root / (name + ".attach.stdout")).open("xb") as out, (root / (name + ".attach.stderr")).open("xb") as err:
            result = subprocess.run(["boss", "attach", request, str(root / name)], stdout=out, stderr=err,
                                    timeout=ATTACHMENT_SECONDS, check=False)
        if result.returncode:
            raise ValueError("native attachment/readback refused for " + name)

    # Only this invocation's private capture is removed after every native
    # attachment has passed downloaded-byte verification. Failures keep all of it.
    for path in root.iterdir():
        path.unlink()
    root.rmdir()


if __name__ == "__main__":
    try:
        retain(sys.argv[1], sys.argv[2], int(sys.argv[3]))
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print("membership observation retention failed: " + str(error), file=sys.stderr)
        sys.exit(74)
