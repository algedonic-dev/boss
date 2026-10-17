#!/usr/bin/env python3
"""Internal guard/judgement for one named READ-ONLY ops verb."""
import json
import os
from pathlib import Path
import re
import stat
import sys


def protected(path, owner):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != owner or info.st_mode & 0o022:
        raise ValueError(f"untrusted regular file: {path}")


def directories(path, stop, owner):
    for parent in path.parents:
        info = parent.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != owner or info.st_mode & 0o022:
            raise ValueError(f"untrusted directory: {parent}")
        if parent == stop:
            break


def guard(source, credential, fixture):
    # Fixture source/credential seams are admitted only after host-check
    # proves every authority seam redirected. Production uses root's export.
    if not fixture and not re.fullmatch(r"/var/lib/boss/tree/gen/[0-9a-f]{40}", str(source)):
        raise ValueError("untrusted source: expected an accepted root generation")
    owner = os.getuid() if fixture else 0
    for name in ("infra/cluster/render-dev-build-policy.py",
                 "infra/cluster/manifests/boss-dev-build-egress.yaml",
                 "infra/estate/estate.toml"):
        path = source / name
        protected(path, owner)
        directories(path, source if fixture else Path("/"), owner)
    protected(credential, owner)
    directories(credential, credential.parent if fixture else Path("/"), owner)
    if stat.S_IMODE(credential.lstat().st_mode) != 0o600:
        raise ValueError("untrusted kubeconfig permissions")


def identity(value, kind, name, namespace):
    metadata = value.get("metadata", {})
    return (value.get("kind") == kind and metadata.get("name") == name
            and metadata.get("namespace") == namespace)


def judge(work):
    expected, actual, cilium = (json.loads((work / f"{name}.json").read_text())
                                for name in ("expected", "actual", "cilium"))
    for value in (expected, actual):
        if (value.get("apiVersion") != "cilium.io/v2"
                or not identity(value, "CiliumNetworkPolicy", "boss-dev-build-egress", "boss-dev")):
            raise ValueError("policy identity mismatch")
    metadata = actual["metadata"]
    if metadata.get("deletionTimestamp") is not None:
        raise ValueError("actual policy is terminating")
    if not all(isinstance(metadata.get(k), str) and metadata[k] for k in ("uid", "resourceVersion")):
        raise ValueError("actual policy UID/resourceVersion missing")
    # Preserve every field. Extra server spec fields are NOT erased to pass.
    # Metadata defaults are irrelevant to equality but remain in raw evidence.
    if not isinstance(expected.get("spec"), dict) or actual.get("spec") != expected["spec"] or "specs" in actual:
        raise ValueError("actual policy spec differs from trusted estate-rendered spec")
    if cilium.get("apiVersion") != "apps/v1" or not identity(cilium, "DaemonSet", "cilium", "kube-system"):
        raise ValueError("Cilium DaemonSet identity mismatch")
    if cilium["metadata"].get("deletionTimestamp") is not None:
        raise ValueError("Cilium DaemonSet is terminating")
    status = cilium.get("status", {})
    generation = cilium["metadata"].get("generation")
    desired = status.get("desiredNumberScheduled")
    # Python booleans compare equal to integers; a malformed observation
    # must not prove readiness (05094098). Missing optional zero counts
    # remain valid Kubernetes snapshots, while an explicit null is refused.
    required_counts = ("observedGeneration", "desiredNumberScheduled",
                       "updatedNumberScheduled", "numberReady", "numberAvailable")
    optional_counts = ("numberUnavailable", "numberMisscheduled")
    if (type(generation) is not int or generation < 1
            or any(type(status.get(k)) is not int for k in required_counts)
            or any(type(status.get(k, 0)) is not int for k in optional_counts)
            or status.get("observedGeneration") != generation
            or type(desired) is not int or desired < 1
            or any(status.get(k) != desired for k in ("updatedNumberScheduled", "numberReady", "numberAvailable"))
            or any(status.get(k, 0) != 0 for k in ("numberUnavailable", "numberMisscheduled"))):
        raise ValueError("Cilium DaemonSet not ready at observed generation")
    print("dev-build-fence-status: installed expected fence; Cilium DaemonSet ready; worker network enforcement remains unproven")


def main():
    try:
        if sys.argv[1] == "guard":
            guard(Path(sys.argv[2]), Path(sys.argv[3]), sys.argv[4] == "fixture")
        elif sys.argv[1] == "judge":
            judge(Path(sys.argv[2]))
        else:
            raise ValueError("unsupported internal operation")
    except (ValueError, OSError, KeyError, TypeError, IndexError) as error:
        print(f"dev-build-fence-status: UNVERIFIED {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
