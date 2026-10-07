"""Recognize retained worker shapes, never authenticate their creating actor.

The generator and this reader consume job.json. Script payloads may belong
to an older generation retained by the Job's TTL; recognition proves no
script provenance, successful execution, or permission to launch anything.
"""
import copy
import json
import re
import sys
from urllib.parse import urlsplit


def binding(token, value):
    if token == "$DEADLINE":
        return type(value) is int and 180 <= value <= 2**64 - 1
    if not isinstance(value, str):
        return False
    if token == "$NAME":
        return re.fullmatch(r"consist-[0-9a-f]{32}", value) is not None
    if token in ("$HEAD", "$BASELINE"):
        return re.fullmatch(r"[0-9a-f]{40}", value) is not None
    if token == "$BUDGET":
        return re.fullmatch(r"[0-9]{1,20}", value) is not None and int(value) <= 2**64 - 1
    if token in ("$CLONE", "$WORKER"):
        return 0 < len(value.encode()) <= 65536 and "\0" not in value
    if token == "$IMAGE":
        return re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:/@-]{0,1023}", value) is not None
    if token == "$REFERENCE":
        return re.fullmatch(r"refs/heads/[A-Za-z0-9][A-Za-z0-9._/-]{0,1023}", value) is not None
    if token == "$URL":
        parsed = urlsplit(value)
        return (len(value) <= 4096 and parsed.scheme in ("http", "https")
                and bool(parsed.hostname) and not parsed.username and not parsed.password
                and not parsed.query and not parsed.fragment and not any(c.isspace() for c in value))
    return False


def matches(expected, actual, bindings):
    if isinstance(expected, str) and expected.startswith("$"):
        if not binding(expected, actual):
            return False
        if expected in bindings and bindings[expected] != actual:
            return False
        bindings[expected] = actual
        return True
    if type(expected) is not type(actual):
        return False
    if isinstance(expected, dict):
        return expected.keys() == actual.keys() and all(
            matches(v, actual[k], bindings) for k, v in expected.items())
    if isinstance(expected, list):
        return len(expected) == len(actual) and all(
            matches(e, a, bindings) for e, a in zip(expected, actual))
    return expected == actual


def remove_default(obj, key, value):
    if key in obj:
        if type(obj[key]) is not type(value) or obj[key] != value:
            return False
        del obj[key]
    return True


def normalized(job):
    job = copy.deepcopy(job)
    job.pop("status", None)
    md = job["metadata"]
    uid, name = md.get("uid"), md.get("name")
    for key in ("creationTimestamp", "generation", "resourceVersion", "uid", "managedFields"):
        md.pop(key, None)
    spec = job["spec"]
    # Only the defaults observed on the real retained Job are removed;
    # changing any value leaves a mismatch rather than broadening a bound.
    for key, value in {"completionMode": "NonIndexed", "completions": 1,
                       "manualSelector": False, "parallelism": 1,
                       "podReplacementPolicy": "TerminatingOrFailed", "suspend": False}.items():
        if not remove_default(spec, key, value):
            return None
    if "selector" in spec:
        if not isinstance(uid, str) or not remove_default(
                spec, "selector", {"matchLabels": {"batch.kubernetes.io/controller-uid": uid}}):
            return None
    labels = spec["template"]["metadata"]["labels"]
    for key, value in {"batch.kubernetes.io/controller-uid": uid,
                       "controller-uid": uid, "batch.kubernetes.io/job-name": name,
                       "job-name": name}.items():
        if key in labels and (not isinstance(value, str) or not remove_default(labels, key, value)):
            return None
    pod = spec["template"]["spec"]
    for key in ("hostPID", "hostIPC", "hostNetwork"):
        pod.setdefault(key, False)  # The API omits explicit false booleans.
    for key, value in {"dnsPolicy": "ClusterFirst", "schedulerName": "default-scheduler",
                       "terminationGracePeriodSeconds": 30}.items():
        if not remove_default(pod, key, value):
            return None
    for container in pod["initContainers"] + pod["containers"]:
        for key, value in {"terminationMessagePath": "/dev/termination-log",
                           "terminationMessagePolicy": "File"}.items():
            if not remove_default(container, key, value):
                return None
    return job


def recognized(template, job):
    try:
        candidate = normalized(job)
        bound = {}
        return (candidate is not None and matches(template, candidate, bound)
                and bound["$DEADLINE"] == min(int(bound["$BUDGET"]) + 180, 2**64 - 1))
    except (KeyError, TypeError, ValueError, AttributeError):
        return False


def validate_template(template):
    # Self-matching proves placeholder coherence, not isolation: a missing
    # safety field matches its own omission. Check the minimum boundary
    # before either reader can answer; job.json still owns the full shape.
    pod = template["spec"]["template"]["spec"]
    if not isinstance(pod, dict):
        raise ValueError("owning worker requires a pod mapping")
    for key in ("automountServiceAccountToken", "shareProcessNamespace",
                "hostPID", "hostIPC", "hostNetwork"):
        if pod.get(key) is not False:
            raise ValueError(f"owning worker requires {key}=false")
    security = pod["securityContext"]
    if (not isinstance(security, dict)
            or not isinstance(security.get("seccompProfile"), dict)
            or security.get("runAsNonRoot") is not True
            or security["seccompProfile"].get("type") != "RuntimeDefault"
            or type(security.get("fsGroup")) is not int or security["fsGroup"] <= 0):
        raise ValueError("owning worker requires non-root default-seccomp pod isolation")
    for role in ("initContainers", "containers"):
        containers = pod[role]
        if not isinstance(containers, list) or not containers:
            raise ValueError(f"owning worker requires {role}")
        for container in containers:
            if not isinstance(container, dict):
                raise ValueError(f"owning worker requires {role} mappings")
            security = container["securityContext"]
            if (not isinstance(security, dict)
                    or not isinstance(security.get("capabilities"), dict)
                    or security.get("allowPrivilegeEscalation") is not False
                    or security.get("readOnlyRootFilesystem") is not True
                    or security.get("privileged", False) is not False
                    or security["capabilities"].get("drop") != ["ALL"]
                    or security["capabilities"].get("add", []) != []
                    or any(type(security.get(key)) is not int or security[key] <= 0
                           for key in ("runAsUser", "runAsGroup"))):
                raise ValueError(f"owning worker requires isolated {role}")
    samples = {"$NAME": "consist-0123456789abcdef0123456789abcdef",
               "$IMAGE": "example.invalid/worker:fixed", "$CLONE": "historical clone",
               "$WORKER": "historical worker", "$URL": "https://example.invalid/repo.git",
               "$REFERENCE": "refs/heads/test", "$HEAD": "1" * 40,
               "$BASELINE": "2" * 40, "$BUDGET": "120", "$DEADLINE": 300}
    used = set()

    def filled(value):
        if isinstance(value, str) and value.startswith("$"):
            used.add(value)
            return samples[value]
        if isinstance(value, list):
            return [filled(v) for v in value]
        if isinstance(value, dict):
            return {k: filled(v) for k, v in value.items()}
        return value

    candidate = filled(template)
    if used != samples.keys() or not recognized(template, candidate):
        raise ValueError("incomplete owning worker template")


def main():
    template = json.load(open(sys.argv[2]))
    validate_template(template)
    if sys.argv[1] == "--declaration":
        # This is a source shape, not a Kubernetes manifest to apply.
        if template["metadata"]["name"] != "$NAME" or template["kind"] != "Job":
            raise ValueError("invalid worker declaration")
        md = template["metadata"]
        labels = md["labels"]
        selector = ",".join(f"{k}={v}" for k, v in sorted(labels.items()))
        if not selector or any("$" in k + str(v) for k, v in labels.items()):
            raise ValueError("worker declaration needs literal labels")
        print(f"Job\t{md['namespace']}\t{selector}\t{sys.argv[3]}")
    elif sys.argv[1] == "--names":
        live = json.load(open(sys.argv[3]))
        if not isinstance(live.get("items"), list):
            raise ValueError("live worker read has no items list")
        for job in live["items"]:
            if recognized(template, job):
                print(job["metadata"]["name"])
    else:
        raise ValueError("unknown declaration operation")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"worker declaration cannot answer: {type(error).__name__}", file=sys.stderr)
        sys.exit(4)
