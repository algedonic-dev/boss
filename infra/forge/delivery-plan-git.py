#!/usr/bin/env python3
"""Read-only command adapter for the shared frozen-plan judgement."""
import argparse
import importlib.util
import json
import pathlib

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--repo", required=True)
parser.add_argument("--base", required=True)
parser.add_argument("--head", required=True)
args = parser.parse_args()
spec = importlib.util.spec_from_file_location("delivery_plan", pathlib.Path(__file__).with_name("delivery-plan.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
print(json.dumps(module.git_plan(args.repo, args.base, args.head), sort_keys=True, separators=(",", ":")))
