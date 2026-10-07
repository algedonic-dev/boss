#!/usr/bin/env python3
"""No-argument retained source-access entry; raw bytes never leave this process."""
import importlib.util
import json
import pathlib
import re
import sys


def unavailable(reason):
    return {"schema": "boss.admission-source-access.v1", "scope": "retained_source_access_probe",
            "history_verdict": "unavailable", "roster": {"state": "unavailable", "reason": reason}, "nodes": []}


def native_limits_available(root):
    """Verify enforced cgroup-v2 ceilings before any protected native read.

    Unsupported kernels are unavailable, never a Docker flag presumed
    effective. Owning controls pin these ceilings to the host's argv.
    """
    values = {}
    try:
        for name in ["memory.max", "memory.swap.max", "pids.max"]:
            with (root / name).open("rb") as handle:
                raw = handle.read(65)
            if re.fullmatch(rb"(?:0|[1-9][0-9]{0,19})\n?", raw) is None:
                return False
            values[name] = int(raw)
    except OSError:
        return False
    return (0 < values["memory.max"] <= 256 * 1024 * 1024
            and values["memory.swap.max"] == 0
            and 0 < values["pids.max"] <= 64)


def main():
    if len(sys.argv) != 1:
        return unavailable("unexpected_arguments")
    if not native_limits_available(pathlib.Path("/sys/fs/cgroup")):
        return unavailable("operator_resource_unavailable")
    # Both code files and the declaration are fixed read-only mounts.
    spec = importlib.util.spec_from_file_location("admission_source", pathlib.Path(__file__).with_name("admission-source.py"))
    source = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(source)
    estate = source.decode(sys.stdin.buffer.read(source.MAX_BYTES + 1))
    with open("/target.json", "rb") as handle:
        target = source.decode(handle.read(source.MAX_BYTES + 1))
    return source.probe_access(estate, target, source.NativeReadPort())


if __name__ == "__main__":
    try:
        report = main()
    except Exception:
        # Missing mounts, tools and unexpected exceptions are unavailable.
        # No traceback or native text can cross the operator boundary.
        report = unavailable("operator_read_unavailable")
    print(json.dumps(report, sort_keys=True, separators=(",", ":"), allow_nan=False))
    measured = report["roster"]["state"] == "matched" and all(node["access"]["state"] == "measured" for node in report["nodes"])
    sys.exit(0 if measured else 4)
