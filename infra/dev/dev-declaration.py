#!/usr/bin/python3 -I
"""Recognize a generated Job from its owning pure factory, never labels alone.

This declares source shape, not creator identity or runtime isolation.
"""
import importlib.util
import json
from pathlib import Path
import sys


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


HERE = Path(__file__).resolve().parent
FACTORY = load("dev_worker_factory", HERE / "dev-build.py")


def recognized(job):
    return FACTORY.native_job_matches(job)


def main():
    if sys.argv[1] == "--declaration":
        if Path(sys.argv[2]).read_bytes() != (HERE / "dev-build.py").read_bytes():
            raise ValueError("owning factory differs from this declaration reader")
        print("Job\tboss-dev\tapp.kubernetes.io/part-of=boss,boss.dev/purpose=isolated-build\tinfra/dev/dev-build.py")
    elif sys.argv[1] == "--names":
        live = json.loads(Path(sys.argv[2]).read_text())
        if not isinstance(live.get("items"), list):
            raise ValueError("live worker read has no items list")
        for job in live["items"]:
            if recognized(job):
                print(job["metadata"]["name"])
    else:
        raise ValueError("unknown declaration operation")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError, IndexError):
        print("dev worker declaration cannot answer", file=sys.stderr)
        sys.exit(4)
