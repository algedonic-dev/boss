#!/usr/bin/env python3
"""Pure delivery-plan judgement shared by CI, deploy and the conductor.

The caller supplies frozen git facts and a pinned delivery-policy row. This
module neither reads a floating branch nor performs publication or apply. A
channel stamp is descriptive; only a complete plan and its native observations
can select the lighter completion path. Unsupported inputs retain image delivery.
"""
import json
import re
import sys
import subprocess
import tomllib
import hashlib


def _sha(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", value) is not None


def _path(value):
    return (isinstance(value, str) and bool(value) and not value.startswith("/")
            and all(part not in ("", ".", "..") for part in value.split("/"))
            and not any(ord(c) < 32 or ord(c) == 127 for c in value))


def plan(facts):
    """Judge complete facts; refusal is an explicit heavier route with cause."""
    def heavy(reason):
        return {"schema": 1, "route": "software", "reason": reason,
                "obligations": [], "source_facts": facts}

    if not isinstance(facts, dict) or not all(_sha(facts.get(k)) for k in ("base", "head")):
        return heavy("exact base/head unavailable")
    policy = facts.get("policy")
    if not isinstance(policy, dict) or not isinstance(policy.get("name"), str) or not policy["name"]:
        return heavy("pinned policy unavailable")
    version = policy.get("version")
    if type(version) is not int or version <= 0:
        return heavy("pinned policy version unavailable")
    channels = policy.get("channels")
    if (not isinstance(channels, dict) or set(channels) - {"schema","enabled","rules","guards"}
            or type(channels.get("schema")) is not int
            or channels["schema"] != 1):
        return heavy("channel policy unavailable")
    if channels.get("enabled") is not True:
        return heavy("lighter delivery is not enabled by the pinned policy")
    if "guards" in channels:
        guards = channels["guards"]
        if (not isinstance(guards, list) or not guards
                or any(not isinstance(g, dict) or set(g) != {"path", "sha256"}
                       or not _path(g.get("path")) or not isinstance(g.get("sha256"), str)
                       or re.fullmatch(r"[0-9a-f]{64}", g["sha256"]) is None for g in guards)):
            return heavy("image-input guard contract malformed")
        if facts.get("guards_match") is not True:
            return heavy("image-input layout differs from the reviewed policy or is unread")
    rules = channels.get("rules")
    if not isinstance(rules, list) or not rules:
        return heavy("channel eligibility unavailable")
    for rule in rules:
        if (not isinstance(rule, dict)
                or not {"prefix", "suffix", "evidence"} <= set(rule)
                or set(rule) - {"prefix", "suffix", "evidence", "protected_names_from", "protected_keys", "allowed_handlers", "excluded_prefixes", "existing_only"}
                or not isinstance(rule.get("prefix"), str)
                or not _path(rule.get("prefix", "").removesuffix("/"))
                or not isinstance(rule.get("suffix"), str) or not rule["suffix"]
                or rule.get("evidence") not in ("forge_blob", "workflow_equality", "rule_equality")):
            return heavy("channel eligibility malformed")
        if "protected_names_from" in rule and (not isinstance(rule["protected_names_from"], str)
                or not _path(rule["protected_names_from"].removesuffix("/"))):
            return heavy("protected family source malformed")
        if "protected_keys" in rule and (not isinstance(rule["protected_keys"], list)
                or not all(isinstance(k, str) and k for k in rule["protected_keys"])):
            return heavy("protected field contract malformed")
        if "allowed_handlers" in rule and (not isinstance(rule["allowed_handlers"], list)
                or not rule["allowed_handlers"]
                or not all(isinstance(k, str) and k for k in rule["allowed_handlers"])):
            return heavy("handler effect contract malformed")
        if "excluded_prefixes" in rule and (not isinstance(rule["excluded_prefixes"], list)
                or not all(isinstance(p, str) and _path(p.removesuffix("/")) for p in rule["excluded_prefixes"])):
            return heavy("excluded image-input paths malformed")
        if "existing_only" in rule and type(rule["existing_only"]) is not bool:
            return heavy("registry lifecycle contract malformed")
    changes = facts.get("changes")
    if not isinstance(changes, list) or not changes:
        return heavy("complete changed-file population unavailable")
    obligations = []
    paths = set()
    for change in changes:
        if not isinstance(change, dict) or set(change) != {"path", "old", "new", "old_mode", "new_mode"}:
            return heavy("changed-file identity malformed")
        path = change["path"]
        if not _path(path) or path in paths:
            return heavy("changed-file path malformed or duplicated")
        paths.add(path)
        for side in ("old", "new"):
            mode = change[side + "_mode"]
            blob = change[side]
            if (not _sha(blob) or mode not in ("100644", "000000")
                    or (mode == "000000") != (set(blob) == {"0"})):
                return heavy("changed-file blob or mode unavailable")
        if change["old"] == change["new"] and change["old_mode"] == change["new_mode"]:
            return heavy("unchanged file presented as a change")
        matching = [r for r in rules if path.startswith(r["prefix"]) and path.endswith(r["suffix"])]
        if len(matching) != 1:
            return heavy("file has no unique lighter evidence contract: " + path)
        if any(path.startswith(p) for p in matching[0].get("excluded_prefixes", [])):
            return heavy("file belongs to an image input: " + path)
        if matching[0].get("existing_only") and "000000" in (change["old_mode"], change["new_mode"]):
            return heavy("new or retired registry names need their runtime bundle transport: " + path)
        if (any(key in matching[0] for key in ("protected_names_from", "protected_keys", "allowed_handlers"))
                and (not isinstance(facts.get("protection"),dict)
                     or facts["protection"].get(path) is not False)):
            return heavy("protected or unmeasured registry effects: " + path)
        obligations.append({**change, "evidence": matching[0]["evidence"]})
    return {"schema": 1, "route": "data", "base": facts["base"], "head": facts["head"],
            "policy": policy, "obligations": sorted(obligations, key=lambda x: x["path"]),
            "source_facts": facts}


def satisfied(delivery_plan, receipts):
    """Every native receipt must bind the exact file, head and evidence kind.

    The adapter obtains observed=true only from a successful native read. The
    complete equality comparator additionally supplies equal=true for registries.
    Repeated observations are not a substitute for a missing file observation.
    """
    if not isinstance(delivery_plan, dict) or delivery_plan.get("route") != "data":
        return False
    obligations = delivery_plan.get("obligations")
    if not isinstance(obligations, list) or not obligations or not isinstance(receipts, list):
        return False
    if len(receipts) != len(obligations):
        return False
    by_path = {}
    for receipt in receipts:
        if not isinstance(receipt, dict) or not _path(receipt.get("path")) or receipt["path"] in by_path:
            return False
        by_path[receipt["path"]] = receipt
    for obligation in obligations:
        receipt = by_path.get(obligation["path"], {})
        if (any(receipt.get(k) != v for k, v in obligation.items())
                or receipt.get("head") != delivery_plan.get("head")
                or receipt.get("observed") is not True):
            return False
        if obligation["evidence"] != "forge_blob" and receipt.get("equal") is not True:
            return False
    return True


def git_plan(repo, base, head):
    """Read the complete no-renames raw diff, never the live branch shortcut.

    Eligibility is read from the base's policy, so this change cannot grant
    itself a new lighter path. A policy edit is itself outside that allowlist.
    Rename detection is deliberately off: both old and new paths need evidence.
    """
    def git(*args):
        return subprocess.check_output(["git", "-C", repo, *args], stderr=subprocess.PIPE)

    try:
        if not all(isinstance(x, str) and x and not x.startswith("-") for x in (repo, base, head)):
            return plan({})
        base = git("rev-parse", "--verify", base + "^{commit}").decode().strip()
        head = git("rev-parse", "--verify", head + "^{commit}").decode().strip()
        git("merge-base", "--is-ancestor", base, head)
        policy_text = git("show", base + ":infra/platform/delivery-policy/train-conductor.toml")
        rows = tomllib.loads(policy_text.decode())["delivery_policy"]
        if len(rows) != 1:
            return plan({})
        row = rows[0]
        policy = {k: row.get(k) for k in ("name", "version", "channels")}
        raw = git("diff", "--raw", "--no-abbrev", "--no-renames", "-z", base, head, "--")
        fields = raw.split(b"\0")
        if fields.pop() != b"" or len(fields) % 2:
            return plan({})
        changes = []
        for index in range(0, len(fields), 2):
            identity = fields[index].decode().split()
            if len(identity) != 5 or not identity[0].startswith(":") or identity[4] not in ("A", "M", "D", "T"):
                return plan({})
            changes.append({"path": fields[index + 1].decode(), "old_mode": identity[0][1:],
                            "new_mode": identity[1], "old": identity[2], "new": identity[3]})
        protection = {}
        for change in changes:
            for rule in (policy.get("channels") or {}).get("rules", []):
                if not isinstance(rule, dict) or not isinstance(rule.get("prefix"), str) or not isinstance(rule.get("suffix"), str):
                    continue
                path = change["path"]
                if not path.startswith(rule["prefix"]) or not path.endswith(rule["suffix"]):
                    continue
                protected = False
                directory = rule.get("protected_names_from")
                if directory:
                    if not isinstance(directory, str) or not _path(directory.removesuffix("/")):
                        return plan({})
                    for commit in (base, head):
                        names = git("ls-tree", "-r", "--name-only", "-z", commit, "--", directory).decode().split("\0")
                        protected |= any(name and name.rsplit("/", 1)[-1].removesuffix(".toml")
                                         == path.rsplit("/", 1)[-1].removesuffix(rule["suffix"]) for name in names)
                if "protected_keys" in rule or "allowed_handlers" in rule:
                    documents = [tomllib.loads(git("cat-file", "blob", change[side]).decode())
                                 if change[side + "_mode"] != "000000" else {} for side in ("old", "new")]
                if "protected_keys" in rule:
                    keys = rule["protected_keys"]
                    if not isinstance(keys, list) or not all(isinstance(k, str) and k for k in keys):
                        return plan({})
                    def protected_values(doc, prefix=()):
                        if isinstance(doc, dict):
                            return [(prefix + (k,), v) for k, v in doc.items() if k in keys] + [
                                item for k, v in doc.items() for item in protected_values(v, prefix + (k,))]
                        if isinstance(doc, list):
                            return [item for i, v in enumerate(doc) for item in protected_values(v, prefix + (i,))]
                        return []
                    protected |= protected_values(documents[0]) != protected_values(documents[1])
                if "allowed_handlers" in rule:
                    allowed = rule["allowed_handlers"]
                    if not isinstance(allowed, list) or not allowed or not all(isinstance(p, str) and p for p in allowed):
                        return plan({})
                    for document in documents:
                        for row in document.get("rule", []):
                            for step in row.get("do", []):
                                handler = step.get("handler")
                                protected |= not isinstance(handler, str) or handler not in allowed
                protection[path] = protected
        guard_observations = []
        for guard in (policy.get("channels") or {}).get("guards", []):
            if not isinstance(guard, dict) or not _path(guard.get("path")):
                return plan({})
            guard_observations.append({"path": guard["path"], "expected": guard.get("sha256"),
                                       **{side: hashlib.sha256(git("show", commit + ":" + guard["path"])).hexdigest()
                                          for side, commit in (("base", base), ("head", head))}})
        return plan({"base": base, "head": head, "policy": policy, "changes": changes,
                     "protection": protection, "guard_observations": guard_observations,
                     "guards_match": all(g["base"] == g["head"] == g["expected"] for g in guard_observations)})
    except (OSError, subprocess.CalledProcessError, UnicodeError, ValueError, KeyError, TypeError, AttributeError) as error:
        stderr = getattr(error, "stderr", b"")
        if isinstance(stderr, bytes):
            stderr = stderr.decode(errors="replace")
        return {**plan({}), "reason": "frozen source read failed",
                "failure": {"kind": type(error).__name__, "message": str(error), "stderr": stderr}}


if __name__ == "__main__":
    request = json.load(sys.stdin)
    if request.get("operation") == "satisfied":
        result = satisfied(request.get("plan"), request.get("receipts"))
    elif request.get("operation") == "git_plan":
        result = git_plan(request.get("repo"), request.get("base"), request.get("head"))
    else:
        result = plan(request)
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
