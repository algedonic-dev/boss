#!/usr/bin/env python3
"""Fixed operator-internal discovery; only the sanitized report leaves the container.

Talos v1.13.8 native schemas: StaticPods (k8s/kube-apiserver),
AuditPolicyConfigs (controlplane/audit-policy). Generated configuration is
not proof of runtime capture, retained records or absence of drops.
The read port is the only side effect; all judgments are functions of data.
"""
import datetime
import gzip
import hashlib
import io
import ipaddress
import json
import os
import posixpath
import re
import selectors
import signal
import subprocess
import sys
import time
import zlib

MAX_BYTES = 1024 * 1024
STREAM_MEMBER_ENCODED = 32 * MAX_BYTES
STREAM_MEMBER_DECODED = 64 * MAX_BYTES
STREAM_PASS_ENCODED = 256 * MAX_BYTES
STREAM_PASS_DECODED = 512 * MAX_BYTES
STREAM_WORK_ENCODED = 2 * STREAM_PASS_ENCODED
STREAM_WORK_DECODED = 2 * STREAM_PASS_DECODED
STREAM_RECORD_BYTES = 256 * 1024
STREAM_PASS_RECORDS = 10000
MAX_NODES = 16
STAGES = {"RequestReceived", "ResponseStarted", "ResponseComplete", "Panic"}
LEVELS = {"None", "Metadata", "Request", "RequestResponse"}
NODE = re.compile(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\Z")
# Talos v1.13.8 constants.KubernetesAPIServerConfigDir plus the filename
# rendered by ControlPlaneConfigController. An extra-arg override is a
# different source; do not attribute this resource's policy to that file.
RENDERED_POLICY = "/system/config/kubernetes/kube-apiserver/auditpolicy.yaml"


class Unavailable(Exception):
    """Fixed reason only; never carry native bytes, paths or exception text."""


def need(condition, reason="unsupported_shape"):
    if not condition:
        raise Unavailable(reason)


def decode(raw):
    need(0 < len(raw) <= MAX_BYTES, "source_size")
    def unique(pairs):
        obj = {}
        for key, value in pairs:
            need(key not in obj, "duplicate_key")
            obj[key] = value
        return obj
    try:
        value = json.loads(raw, object_pairs_hook=unique,
                           parse_constant=lambda _: (_ for _ in ()).throw(Unavailable("nonfinite_json")))
        need(isinstance(value, dict), "source_shape")
        return value
    except Unavailable:
        raise
    except (ValueError, UnicodeError, RecursionError):
        raise Unavailable("invalid_json") from None


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"),
                                     allow_nan=False).encode()).hexdigest()


def unknown(reason):
    return {"state": "unavailable", "reason": reason}


def strings(value):
    need(isinstance(value, list) and len(value) <= 256
         and all(isinstance(x, str) and len(x) <= 512 for x in value))
    return value


def estate_roster(estate):
    rows = estate.get("data")
    need(isinstance(rows, list) and 0 < len(rows) <= 256, "estate_shape")
    ids = set()
    addresses = set()
    selected = []
    for row in rows:
        need(isinstance(row, dict), "estate_shape")
        ident = row.get("id")
        need(isinstance(ident, str) and NODE.fullmatch(ident) and ident not in ids, "estate_identity")
        ids.add(ident)
        need(isinstance(row.get("retired"), bool), "estate_retirement")
        if row.get("role") != "talos-control-plane" or row["retired"]:
            continue
        address = row.get("address")
        try:
            ip = str(ipaddress.IPv4Address(address))
        except (ValueError, TypeError):
            raise Unavailable("estate_address") from None
        need(ip == address and ip not in addresses, "estate_address")
        addresses.add(ip)
        selected.append({"node": ident, "address": ip})
    need(0 < len(selected) <= MAX_NODES, "estate_control_plane_count")
    return sorted(selected, key=lambda r: r["node"])


def agree(roster, nodes):
    # kubectl v1.33.3 printGeneric wraps get nodes -o json in v1 List.
    # Its wrapper is not a node identity: validate every item before
    # selecting control planes, including items outside that selection.
    need(nodes.get("kind") in ["NodeList", "List"] and nodes.get("apiVersion") == "v1",
         "native_roster_shape")
    metadata = nodes.get("metadata", {})
    need(isinstance(metadata, dict), "native_roster_shape")
    continuation = metadata.get("continue", "")
    remaining = metadata.get("remainingItemCount", 0)
    need(isinstance(continuation, str) and continuation == ""
         and type(remaining) is int and remaining == 0,
         "native_roster_incomplete")
    items = nodes.get("items")
    need(isinstance(items, list) and len(items) <= 256, "native_roster_shape")
    names = set()
    actual = {}
    for item in items:
        need(isinstance(item, dict) and item.get("kind") == "Node" and item.get("apiVersion") == "v1",
             "native_roster_shape")
        meta = item.get("metadata", {})
        need(isinstance(meta, dict), "native_roster_shape")
        name = meta.get("name")
        need(isinstance(name, str) and NODE.fullmatch(name) and name not in names, "native_roster_identity")
        names.add(name)
        labels = meta.get("labels", {})
        need(isinstance(labels, dict), "native_roster_shape")
        if not ({"node-role.kubernetes.io/control-plane", "node-role.kubernetes.io/master"} & labels.keys()):
            continue
        uid = meta.get("uid")
        need(isinstance(uid, str) and 0 < len(uid) <= 128, "native_roster_identity")
        status = item.get("status", {})
        need(isinstance(status, dict), "native_roster_shape")
        addrs = status.get("addresses", [])
        need(isinstance(addrs, list), "native_roster_shape")
        ips = [a.get("address") for a in addrs if isinstance(a, dict) and a.get("type") == "InternalIP"]
        need(len(ips) == 1, "native_roster_address")
        actual[name] = (ips[0], uid)
    need(set(actual) == {r["node"] for r in roster}, "control_plane_roster_mismatch")
    for row in roster:
        need(actual[row["node"]][0] == row["address"], "control_plane_address_mismatch")
    return [dict(row, native_identity=digest({"uid": actual[row["node"]][1]})) for row in roster]


def target_valid(target):
    need(set(target) == {"policy", "binding", "namespace", "user", "groups", "requests", "stages"}, "target_shape")
    for key in ["policy", "binding", "namespace", "user"]:
        need(isinstance(target[key], str) and 0 < len(target[key]) <= 256, "target_shape")
    strings(target["groups"])
    need(strings(target["stages"]) and set(target["stages"]) <= STAGES, "target_stages")
    requests = target["requests"]
    need(isinstance(requests, list) and 0 < len(requests) <= 32, "target_shape")
    for request in requests:
        need(isinstance(request, dict) and set(request) == {"group", "resource", "verb"}, "target_shape")
        need(all(isinstance(v, str) and len(v) <= 128 for v in request.values()), "target_shape")


def resource_matches(resource, request):
    # Kubernetes audit/policy/checker.go, not RBAC. The validator refuses
    # version-dependent wildcard groups. resource/* matches its parent;
    # */status requires a subresource. The caller keeps first-match order.
    if resource.get("group", "") != request["group"]:
        return False
    patterns = resource.get("resources", [])
    if not patterns:
        return True
    combined = request["resource"]
    parent, _, subresource = combined.partition("/")
    return any(pattern == combined or pattern == "*"
               or (subresource and pattern.startswith("*/") and subresource == pattern[2:])
               or (pattern.endswith("/*") and parent == pattern[:-2]) for pattern in patterns)


def capture_coverage(policy, target):
    need(set(policy) <= {"apiVersion", "kind", "metadata", "rules", "omitStages", "omitManagedFields"})
    need(policy.get("kind") == "Policy" and policy.get("apiVersion") == "audit.k8s.io/v1")
    global_omit = strings(policy.get("omitStages", []))
    need(set(global_omit) <= STAGES)
    if "omitManagedFields" in policy:
        need(isinstance(policy["omitManagedFields"], bool))
    rules = policy.get("rules")
    need(isinstance(rules, list) and 0 < len(rules) <= 256)
    # Validate the whole policy before selecting a rule. An unreadable rule
    # cannot disappear just because a prior target happened to match.
    for rule in rules:
        need(isinstance(rule, dict) and set(rule) <= {"level", "users", "userGroups", "verbs", "resources", "namespaces", "nonResourceURLs", "omitStages", "omitManagedFields"})
        need(isinstance(rule.get("level"), str) and rule["level"] in LEVELS)
        for key in ["users", "userGroups", "verbs", "namespaces", "nonResourceURLs", "omitStages"]:
            strings(rule.get(key, []))
        need(set(rule.get("omitStages", [])) <= STAGES)
        if "omitManagedFields" in rule:
            need(isinstance(rule["omitManagedFields"], bool))
        resources = rule.get("resources", [])
        need(isinstance(resources, list) and len(resources) <= 256)
        need(not ((resources or rule.get("namespaces")) and rule.get("nonResourceURLs")))
        for resource in resources:
            need(isinstance(resource, dict) and set(resource) <= {"group", "resources", "resourceNames"})
            need(isinstance(resource.get("group", ""), str))
            # v1.33 compares groups literally; v1.36 also recognizes '*'.
            # Generated Pod configuration does not establish the running
            # server's matcher version, so this capability is unavailable.
            need(resource.get("group") != "*", "audit_group_wildcard_version_unavailable")
            strings(resource.get("resources", []))
            strings(resource.get("resourceNames", []))
            # Targets describe every object of a resource. Named subsets
            # cannot establish full coverage and are not guessed away.
            need(not resource.get("resourceNames"), "named_resource_coverage_unavailable")
    covered = []
    for request in target["requests"]:
        match = None
        for rule in rules:
            if rule.get("users") and target["user"] not in rule["users"]:
                continue
            if rule.get("userGroups") and not (set(rule["userGroups"]) & set(target["groups"])):
                continue
            if rule.get("verbs") and request["verb"] not in rule["verbs"]:
                continue
            if rule.get("namespaces") and target["namespace"] not in rule["namespaces"]:
                continue
            if rule.get("nonResourceURLs"):
                continue
            if rule.get("resources") and not any(resource_matches(resource, request) for resource in rule["resources"]):
                continue
            match = rule
            break
        covered.append(match is not None and match["level"] != "None"
                       and not (set(target["stages"]) & (set(global_omit) | set(match.get("omitStages", [])))))
    return {"state": "covered" if all(covered) else "not_covered",
            "request_count": len(covered), "covered_count": sum(covered),
            "target_identity": digest(target), "stage_count": len(target["stages"])}


def native_body(obj, address, kind, ident, namespace):
    meta = obj.get("metadata", {})
    need(isinstance(meta, dict) and obj.get("node") == address and meta.get("type") == kind
         and meta.get("id") == ident and meta.get("namespace") == namespace, "native_resource_identity")
    spec = obj.get("spec")
    need(isinstance(spec, dict), "native_resource_shape")
    return spec


def configuration(obj, address):
    pod = native_body(obj, address, "StaticPods.kubernetes.talos.dev", "kube-apiserver", "k8s")
    need(isinstance(pod.get("metadata"), dict) and isinstance(pod.get("spec"), dict), "native_pod_shape")
    need(pod.get("kind") == "Pod" and pod.get("apiVersion") == "v1"
         and pod.get("metadata", {}).get("name") == "kube-apiserver", "native_pod_shape")
    containers = pod.get("spec", {}).get("containers")
    need(isinstance(containers, list), "native_pod_shape")
    selected = [c for c in containers if isinstance(c, dict) and c.get("name") == "kube-apiserver"]
    need(len(selected) == 1, "native_pod_shape")
    container = selected[0]
    command = strings(container.get("command", []))
    args = strings(container.get("args", []))
    need(command and command[0] in ["kube-apiserver", "/usr/local/bin/kube-apiserver"], "native_pod_command")
    flags = {}
    tokens = command[1:] + args
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if token.startswith("--audit-"):
            key, equal, value = token.partition("=")
            if not equal:
                index += 1
                need(index < len(tokens) and not tokens[index].startswith("--"), "audit_argument_shape")
                value = tokens[index]
            need(key not in flags, "duplicate_audit_argument")
            flags[key] = value
        index += 1
    policy_path = flags.get("--audit-policy-file", "")
    log_path = flags.get("--audit-log-path", "")
    webhook = flags.get("--audit-webhook-config-file", "")
    sinks = []
    if log_path:
        sinks.append({"class": "stdout" if log_path == "-" else "file", "identity": digest(log_path)})
    if webhook:
        sinks.append({"class": "webhook_configuration", "identity": digest(webhook)})
    retention = {}
    for flag, name in [("--audit-log-maxage", "max_age_days"), ("--audit-log-maxbackup", "max_backups"), ("--audit-log-maxsize", "max_size_megabytes")]:
        raw = flags.get(flag)
        if raw is None:
            retention[name] = unknown("effective_default_not_measured")
        else:
            need(re.fullmatch(r"[0-9]{1,9}", raw) is not None, "audit_retention_shape")
            retention[name] = {"state": "configured", "value": int(raw)}
    result = {"provenance": "talos_generated_static_pod",
              "audit": "configured_enabled" if policy_path and sinks else "configured_disabled",
              "sinks": sinks, "retention": retention,
              "policy_file_identity": digest(policy_path) if policy_path else None}
    return result, policy_path == RENDERED_POLICY



def retained_sink(obj, address):
    """Derive only a configured audit log's host mapping, never caller paths.

    The path is protected operator-internal data. An unsupported mapping
    refuses instead of falling back to a guessed host path or another file.
    """
    pod = native_body(obj, address, "StaticPods.kubernetes.talos.dev", "kube-apiserver", "k8s")
    need(pod.get("kind") == "Pod" and pod.get("apiVersion") == "v1", "native_pod_shape")
    need(isinstance(pod.get("metadata"), dict) and pod["metadata"].get("name") == "kube-apiserver", "native_pod_shape")
    spec = pod.get("spec")
    need(isinstance(spec, dict), "native_pod_shape")
    containers = spec.get("containers")
    need(isinstance(containers, list), "native_pod_shape")
    selected = [c for c in containers if isinstance(c, dict) and c.get("name") == "kube-apiserver"]
    need(len(selected) == 1, "native_pod_shape")
    container = selected[0]
    command = strings(container.get("command", []))
    need(command and command[0] in ["kube-apiserver", "/usr/local/bin/kube-apiserver"], "native_pod_command")
    args = command[1:] + strings(container.get("args", []))
    paths = []
    for index, arg in enumerate(args):
        if arg == "--audit-log-path":
            need(index + 1 < len(args), "audit_argument_shape")
            paths.append(args[index + 1])
        elif arg.startswith("--audit-log-path="):
            paths.append(arg.split("=", 1)[1])
    need(len(paths) == 1, "audit_argument_shape")

    def canonical(path):
        return (isinstance(path, str) and 0 < len(path) <= 512
                and re.fullmatch(r"/[A-Za-z0-9_./-]+", path) is not None
                and path == posixpath.normpath(path)
                and ".." not in path.split("/") and not path.startswith("//"))

    path = paths[0]
    need(canonical(path) and path.endswith(".log"), "retained_sink_unavailable")
    mounts = container.get("volumeMounts", [])
    volumes = spec.get("volumes", [])
    need(isinstance(mounts, list) and isinstance(volumes, list), "retained_sink_unavailable")
    def volume_name(value):
        return (isinstance(value, str) and 0 < len(value) <= 63
                and re.fullmatch(r"[a-z0-9](?:[a-z0-9-]*[a-z0-9])?", value) is not None)

    # Missing identities used to join None to None and select protected
    # bytes from an invalid native graph (independent review dbcacb98).
    need(all(isinstance(v, dict) and volume_name(v.get("name")) for v in volumes),
         "retained_sink_unavailable")
    names = [v["name"] for v in volumes]
    need(len(names) == len(set(names)), "retained_sink_unavailable")
    need(all(isinstance(m, dict) and volume_name(m.get("name")) and m["name"] in names
             for m in mounts), "retained_sink_unavailable")
    candidates = [m for m in mounts if isinstance(m, dict) and canonical(m.get("mountPath"))
                  and path.startswith(m["mountPath"].rstrip("/") + "/")]
    need(len(candidates) == 1, "retained_sink_unavailable")
    mount = candidates[0]
    need(not mount.get("subPath") and not mount.get("subPathExpr"), "retained_sink_unavailable")
    matched = [v for v in volumes if isinstance(v, dict) and v.get("name") == mount.get("name")]
    need(len(matched) == 1, "retained_sink_unavailable")
    need(set(matched[0]) == {"name", "hostPath"}, "retained_sink_unavailable")
    host = matched[0].get("hostPath")
    need(isinstance(host, dict) and canonical(host.get("path")), "retained_sink_unavailable")
    need(host["path"].startswith("/var/log/") and host.get("type") in [None, "Directory", "DirectoryOrCreate"],
         "retained_sink_unavailable")
    result = host["path"].rstrip("/") + path[len(mount["mountPath"].rstrip("/")):]
    need(canonical(result), "retained_sink_unavailable")
    return result


def retained_members(inventory, sink, max_backups, member_bound=MAX_BYTES):
    """Select only the configured file and pinned native rotation family."""
    need(isinstance(max_backups, int) and not isinstance(max_backups, bool) and 0 < max_backups <= 16,
         "retained_rotation_bound")
    basename = posixpath.basename(sink)
    stem, extension = posixpath.splitext(basename)
    seen = set()
    selected = []
    for row in inventory:
        name = row["name"]
        if name == basename:
            key = "current"
        elif name.startswith(stem + "-") or name.startswith(basename + "."):
            match = re.fullmatch(re.escape(stem) + r"-([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}-[0-9]{2}-[0-9]{2}\.[0-9]{3})"
                                 + re.escape(extension) + r"(?:\.gz)?", name)
            need(match is not None, "retained_rotation_shape")
            try:
                datetime.datetime.strptime(match[1], "%Y-%m-%dT%H-%M-%S.%f")
            except ValueError:
                raise Unavailable("retained_rotation_shape") from None
            key = match[1]
        else:
            continue
        need(key not in seen, "retained_rotation_duplicate")
        seen.add(key)
        need(row["size"] > 0, "retained_member_empty")
        need(row["size"] <= member_bound, "retained_member_size_bound")
        selected.append(row)
    need("current" in seen and len(selected) <= max_backups + 1, "retained_rotation_incomplete")
    need(sum(row["size"] for row in selected) <= 8 * member_bound, "retained_population_size")
    return selected


def retained_payload(raw, compressed):
    need(isinstance(raw, bytes) and 0 < len(raw) <= MAX_BYTES, "retained_file_size")
    if not compressed:
        return raw
    try:
        with gzip.GzipFile(fileobj=io.BytesIO(raw), mode="rb") as source:
            value = source.read(MAX_BYTES + 1)
        need(len(value) > 0, "retained_decoded_empty")
        need(len(value) <= MAX_BYTES, "retained_decoded_size_bound")
        return value
    except (OSError, EOFError, zlib.error):
        raise Unavailable("retained_compression_unavailable") from None


class StreamBudget:
    """Approved ad504 conservative work ceiling; no retries or raw scratch."""
    def __init__(self):
        self.deadline = time.monotonic() + 850
        self.encoded = 0
        self.decoded = 0
        self.planned = 0
        self.pass_encoded = [0, 0]
        self.pass_decoded = [0, 0]
        self.pass_records = [0, 0]

    def reserve(self, members):
        self.planned += sum(member["size"] for member in members)
        need(self.planned <= STREAM_PASS_ENCODED, "retained_population_size")

    def charge(self, encoded=0, decoded=0, pass_index=0):
        need(time.monotonic() < self.deadline, "retained_stream_time_bound")
        self.encoded += encoded
        self.decoded += decoded
        self.pass_encoded[pass_index] += encoded
        self.pass_decoded[pass_index] += decoded
        need(self.pass_encoded[pass_index] <= STREAM_PASS_ENCODED, "retained_population_size")
        need(self.pass_decoded[pass_index] <= STREAM_PASS_DECODED, "retained_stream_decoded_population_bound")
        need(self.encoded <= STREAM_WORK_ENCODED, "retained_stream_work_bound")
        need(self.decoded <= STREAM_WORK_DECODED, "retained_stream_decoded_work_bound")


def stream_member(port, argv, compressed, budget, seen, pass_index=0):
    """Consume every encoded byte and gzip trailer, retaining only bounded lines."""
    encoded = decoded = records = 0
    identity = hashlib.sha256()
    pending = bytearray()
    first = last = None
    decoder = zlib.decompressobj(16 + zlib.MAX_WBITS) if compressed else None
    gzip_complete = False

    def consume(chunk):
        nonlocal decoded, records, first, last
        budget.charge(decoded=len(chunk), pass_index=pass_index)
        decoded += len(chunk)
        need(decoded <= STREAM_MEMBER_DECODED, "retained_stream_decoded_member_bound")
        pending.extend(chunk)
        while True:
            end = pending.find(b"\n")
            if end < 0:
                need(len(pending) <= STREAM_RECORD_BYTES, "retained_stream_record_bound")
                break
            need(end + 1 <= STREAM_RECORD_BYTES, "retained_stream_record_bound")
            line = bytes(pending[:end + 1])
            del pending[:end + 1]
            need(len(seen) < STREAM_PASS_RECORDS, "retained_record_count")
            budget.pass_records[pass_index] += 1
            need(budget.pass_records[pass_index] <= STREAM_PASS_RECORDS, "retained_record_count")
            value = retained_records(line, seen)
            records += value["records"]
            at = value["first_retained"]
            until = value["last_retained"]
            if first is None or audit_time(at) < audit_time(first):
                first = at
            if last is None or audit_time(until) > audit_time(last):
                last = until

    stream = port.stream_bytes(argv, budget.deadline)
    try:
        for chunk in stream:
            need(isinstance(chunk, bytes) and 0 < len(chunk) <= 65536, "retained_stream_chunk_shape")
            budget.charge(encoded=len(chunk), pass_index=pass_index)
            encoded += len(chunk)
            need(encoded <= STREAM_MEMBER_ENCODED, "retained_stream_member_bound")
            identity.update(chunk)
            if not compressed:
                consume(chunk)
                continue
            tail = chunk
            while tail:
                if gzip_complete:
                    decoder = zlib.decompressobj(16 + zlib.MAX_WBITS)
                    gzip_complete = False
                value = decoder.decompress(tail, 65536)
                consume(value)
                tail = decoder.unconsumed_tail
                if decoder.eof:
                    gzip_complete = True
                    tail = decoder.unused_data
        need(encoded > 0, "retained_member_empty")
        need(not compressed or gzip_complete, "retained_compression_unavailable")
        need(decoded > 0, "retained_decoded_empty")
        need(not pending and records > 0, "retained_records_incomplete")
        return {"encoded_bytes": encoded, "decoded_bytes": decoded,
                "content_identity": identity.hexdigest(), "records": records,
                "first_retained": first, "last_retained": last}
    except zlib.error:
        raise Unavailable("retained_compression_unavailable") from None
    finally:
        stream.close()


def audit_time(value):
    """Validate native UTC audit timestamps without losing nanoseconds."""
    need(isinstance(value, str), "retained_record_timestamp")
    match = re.fullmatch(r"([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2})(?:\.([0-9]{1,9}))?Z", value)
    need(match is not None, "retained_record_timestamp")
    try:
        parsed = datetime.datetime.strptime(match[1], "%Y-%m-%dT%H:%M:%S")
    except ValueError:
        raise Unavailable("retained_record_timestamp") from None
    delta = parsed - datetime.datetime(1970, 1, 1)
    need(delta.days >= 0, "retained_record_timestamp")
    return (delta.days * 86400 + delta.seconds) * 10**9 + int((match[2] or "").ljust(9, "0"))


def retained_records(raw, seen=None):
    """Measure all complete native records; export no identities or payloads.

    This establishes retained byte/record access, never absence of dropped
    requests or a complete requested historical interval.
    """
    need(isinstance(raw, bytes) and 0 < len(raw) <= MAX_BYTES and raw.endswith(b"\n"), "retained_records_incomplete")
    lines = raw.splitlines()
    need(0 < len(lines) <= STREAM_PASS_RECORDS, "retained_record_count")
    if seen is None:
        seen = set()
    first = None
    last = None
    for line in lines:
        event = decode(line)
        need(event.get("apiVersion") == "audit.k8s.io/v1" and event.get("kind") == "Event",
             "retained_record_shape")
        need(event.get("level") in ["Metadata", "Request", "RequestResponse"] and event.get("stage") in STAGES,
             "retained_record_shape")
        ident = event.get("auditID")
        need(isinstance(ident, str) and re.fullmatch(r"[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}", ident) is not None,
             "retained_record_identity")
        pair = (ident, event["stage"])
        need(pair not in seen, "retained_record_duplicate")
        seen.add(pair)
        start = event.get("requestReceivedTimestamp")
        end = event.get("stageTimestamp")
        lo, hi = audit_time(start), audit_time(end)
        need(lo <= hi, "retained_record_timestamp")
        if first is None or lo < first[0]:
            first = (lo, start)
        if last is None or hi > last[0]:
            last = (hi, end)
    return {"records": len(lines), "first_retained": first[1], "last_retained": last[1],
            "identity": hashlib.sha256(raw).hexdigest()}


def retained_inventory(raw, address):
    """Parse pinned v1.13.8 --long output inside the protected operator.

    The CLI discards timestamp precision/year in display; preserve that
    display for stability comparisons, never treat it as an exact bound.
    No row may disappear before the whole listing is validated.
    """
    need(isinstance(raw, bytes) and 0 < len(raw) <= MAX_BYTES, "retained_inventory_size")
    try:
        lines = raw.decode("utf-8", errors="strict").splitlines()
    except UnicodeError:
        raise Unavailable("retained_inventory_shape") from None
    need(1 < len(lines) <= 257 and lines[0].split() ==
         ["NODE", "MODE", "UID", "GID", "SIZE(B)", "LASTMOD", "LABEL", "NAME"], "retained_inventory_shape")
    seen = set()
    entries = []
    root_seen = False
    for line in lines[1:]:
        columns = re.split(r"[ ]{3,}", line.strip())
        need(len(columns) in [7, 8], "retained_inventory_shape")
        node, mode, uid, gid, size, modified = columns[:6]
        label = columns[6] if len(columns) == 8 else ""
        name = columns[-1]
        need(node == address, "native_resource_identity")
        need(re.fullmatch(r"[-d][rwx-]{9}", mode) is not None, "retained_inventory_type")
        need(all(re.fullmatch(r"[0-9]{1,19}", value) is not None for value in [uid, gid, size]), "retained_inventory_shape")
        need(int(size) <= 2**63 - 1 and int(uid) <= 2**32 - 1 and int(gid) <= 2**32 - 1,
             "retained_inventory_size")
        need(re.fullmatch(r"(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) [ 0-9][0-9] (?:[0-9]{2}:[0-9]{2}:[0-9]{2}|[0-9]{4} [0-9]{2}:[0-9]{2})", modified) is not None,
             "retained_inventory_shape")
        try:
            # Supply a leap year only to validate the yearless CLI display;
            # it remains a display string, not a recovered timestamp.
            if len(modified.split()) == 3:
                datetime.datetime.strptime(modified + " 2000", "%b %d %H:%M:%S %Y")
            else:
                datetime.datetime.strptime(modified, "%b %d %Y %H:%M")
        except ValueError:
            raise Unavailable("retained_inventory_shape") from None
        need(len(label) <= 512 and (not label or re.fullmatch(r"[A-Za-z0-9_:.,/-]+", label) is not None),
             "retained_inventory_shape")
        need(re.fullmatch(r"[A-Za-z0-9_.-]{1,255}", name) is not None and name != ".." and name not in seen,
             "retained_inventory_identity")
        seen.add(name)
        if name == ".":
            need(mode.startswith("d"), "retained_inventory_type")
            root_seen = True
        else:
            # A nested directory cannot be silently treated as complete
            # history. Unsupported contributors stop the measurement.
            need(mode.startswith("-"), "retained_inventory_type")
            entries.append({"name": name, "size": int(size), "mode": mode,
                            "uid": int(uid), "gid": int(gid),
                            "modified_display": modified, "label": label})
    need(root_seen and entries, "retained_inventory_incomplete")
    return sorted(entries, key=lambda item: item["name"])


def empty_node(row, reason):
    return {"node": row["node"], "native_identity": row["native_identity"],
            "configuration": {"provenance": "talos_generated_static_pod", "audit": "unknown", "reason": reason},
            "capture_policy": unknown(reason),
            **{key: unknown("retained_source_access_not_established") for key in
               ["runtime_capture", "first_retained", "last_retained", "rotations", "drops", "errors", "completeness"]}}


def discover(estate, target, port):
    result = {"schema": "boss.admission-source-discovery.v1", "scope": "configuration_discovery_only",
              "history_verdict": "unavailable", "roster": unknown("not_measured"), "nodes": []}
    try:
        target_valid(target)
        roster = estate_roster(estate)
        rows = agree(roster, port.read(["kubectl", "--kubeconfig=/kc", "get", "nodes", "-o", "json", "--request-timeout=20s"]))
    except Unavailable as error:
        result["roster"] = unknown(str(error))
        return result
    result["roster"] = {"state": "matched", "registered_count": len(rows), "native_count": len(rows),
                        "identity": digest(rows)}
    for row in rows:
        node = empty_node(row, "not_measured")
        address = row["address"]
        base = ["/talosctl", "--talosconfig=/tc", "-n", address, "get"]
        try:
            config, enabled_policy = configuration(port.read(base + ["StaticPods.kubernetes.talos.dev", "kube-apiserver", "--namespace", "k8s", "-o", "json"]), address)
            node["configuration"] = config
        except Unavailable as error:
            node["configuration"]["reason"] = str(error)
            enabled_policy = None
        # An unavailable pod does not drop its contributor or suppress the
        # independent policy measurement; a disabled policy does not claim coverage.
        try:
            policy = native_body(port.read(base + ["AuditPolicyConfigs.kubernetes.talos.dev", "audit-policy", "--namespace", "controlplane", "-o", "json"]),
                                 address, "AuditPolicyConfigs.kubernetes.talos.dev", "audit-policy", "controlplane").get("config")
            need(isinstance(policy, dict), "native_policy_shape")
            coverage = capture_coverage(policy, target)
            node["capture_policy"] = {"state": "measured_configuration", "identity": digest(policy), "coverage": coverage}
            if enabled_policy is not True:
                node["capture_policy"]["coverage"] = unknown("effective_policy_not_established")
        except Unavailable as error:
            node["capture_policy"] = {"state": "unavailable", "reason": str(error), "coverage": unknown(str(error))}
        result["nodes"].append(node)
    return result


def probe_access(estate, target, port):
    """Fixed method under existing authority; only sanitized evidence leaves."""
    budget = StreamBudget()
    discovery = discover(estate, target, port)
    result = {"schema": "boss.admission-source-access.v1", "scope": "retained_source_access_probe",
              "history_verdict": "unavailable", "roster": discovery["roster"], "nodes": []}
    if discovery["roster"]["state"] != "matched":
        return result
    roster_command = ["kubectl", "--kubeconfig=/kc", "get", "nodes", "-o", "json", "--request-timeout=20s"]
    try:
        rows = agree(estate_roster(estate), port.read(roster_command))
        need(digest(rows) == discovery["roster"]["identity"], "retained_source_unstable")
    except Unavailable as error:
        result["roster"] = unknown(str(error))
        return result
    for row, configuration_node in zip(rows, discovery["nodes"], strict=True):
        node = {"node": row["node"], "native_identity": row["native_identity"],
                "access": unknown("not_measured"), "sink_identity": None,
                "drops": unknown("source_drop_coverage_unavailable"),
                "errors": unknown("source_error_coverage_unavailable"),
                "completeness": unknown("historical_continuity_unavailable")}
        base = ["/talosctl", "--talosconfig=/tc", "-n", row["address"]]
        pod_command = base + ["get", "StaticPods.kubernetes.talos.dev", "kube-apiserver", "--namespace", "k8s", "-o", "json"]
        try:
            config = configuration_node["configuration"]
            policy = configuration_node["capture_policy"]
            need(config["audit"] == "configured_enabled" and policy.get("coverage", {}).get("state") == "covered",
                 "effective_policy_not_established")
            need(all(value["state"] == "configured" and value["value"] > 0 for value in config["retention"].values()),
                 "retained_rotation_bound")
            pod = port.read(pod_command)
            current, policy_bound = configuration(pod, row["address"])
            need(current == config and policy_bound, "retained_source_unstable")
            sink = retained_sink(pod, row["address"])
            node["sink_identity"] = digest(sink)
            inventory_command = base + ["list", "--long", posixpath.dirname(sink)]
            inventory = retained_inventory(port.read_bytes(inventory_command), row["address"])
            members = retained_members(inventory, sink, config["retention"]["max_backups"]["value"],
                                       STREAM_MEMBER_ENCODED)
            budget.reserve(members)
            measured = []
            raw_identities = []
            seen = set()
            for member in members:
                value = stream_member(port, base + ["read", posixpath.join(posixpath.dirname(sink), member["name"])],
                                      member["name"].endswith(".gz"), budget, seen)
                need(value["encoded_bytes"] == member["size"], "retained_source_unstable")
                raw_identities.append(value["content_identity"])
                measured.append(value)
            after = retained_inventory(port.read_bytes(inventory_command), row["address"])
            need(inventory == after, "retained_source_unstable")
            replay_seen = set()
            for member, expected in zip(members, measured, strict=True):
                value = stream_member(port, base + ["read", posixpath.join(posixpath.dirname(sink), member["name"])],
                                      member["name"].endswith(".gz"), budget, replay_seen, pass_index=1)
                need(value == expected, "retained_source_unstable")
            need(port.read(pod_command) == pod, "retained_source_unstable")
            # A policy configuration can change during a retained read too.
            policy_object = port.read(base + ["get", "AuditPolicyConfigs.kubernetes.talos.dev", "audit-policy", "--namespace", "controlplane", "-o", "json"])
            actual_policy = native_body(policy_object, row["address"], "AuditPolicyConfigs.kubernetes.talos.dev", "audit-policy", "controlplane").get("config")
            need(isinstance(actual_policy, dict) and digest(actual_policy) == policy["identity"], "retained_source_unstable")
            # The second read can rotate a sibling while preserving every
            # original member's bytes. Check the whole population after it,
            # not only the frozen names selected before that read (ad504).
            need(retained_inventory(port.read_bytes(inventory_command), row["address"]) == inventory,
                 "retained_source_unstable")
            budget.charge()
            node["access"] = {"state": "measured", "file_count": len(members), "rotation_count": len(members)-1,
                              "bytes": sum(member["size"] for member in members),
                              "records": sum(item["records"] for item in measured),
                              "first_retained": min((item["first_retained"] for item in measured), key=audit_time),
                              "last_retained": max((item["last_retained"] for item in measured), key=audit_time),
                              "inventory_identity": digest(inventory), "content_identity": digest(raw_identities),
                              "continuity": "stable_two_reads_only"}
        except Unavailable as error:
            node["access"] = unknown(str(error))
        result["nodes"].append(node)
    try:
        need(agree(estate_roster(estate), port.read(roster_command)) == rows, "retained_source_unstable")
        budget.charge()
    except Unavailable as error:
        result["roster"] = unknown(str(error))
        for node in result["nodes"]:
            node["access"] = unknown(str(error))
    return result


class NativeReadPort:
    """Protected stdout/stderr are bounded and transient inside this process.

    No shell, dynamic command, path fallback, native-error echo or raw file.
    The process group is killed on timeout/overflow so neither a child nor
    its inherited pipes can keep the operator running beyond its bound.
    """
    def __init__(self, timeout=25):
        need(isinstance(timeout, (int, float)) and 0 < timeout <= 25, "native_read_timeout")
        self.timeout = timeout

    def read(self, argv):
        return decode(self._read(argv, strict_diagnostics=False))

    def read_bytes(self, argv):
        """Retained-file/list reads refuse even nonfatal native diagnostics.

        Bytes remain transient in this operator process; the caller's fixed
        adapter supplies the command and sanitizes before any output.
        """
        return self._read(argv, strict_diagnostics=True)

    def stream_bytes(self, argv, deadline):
        """Bound native pipe chunks, diagnostics and lifetime without collecting a file."""
        try:
            process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE, start_new_session=True)
        except OSError:
            raise Unavailable("native_read_failed") from None
        deadline = min(deadline, time.monotonic() + self.timeout)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ, True)
                selector.register(process.stderr, selectors.EVENT_READ, False)
                while selector.get_map():
                    remaining = deadline - time.monotonic()
                    need(remaining > 0, "native_read_timeout")
                    for key, _ in selector.select(min(remaining, 1)):
                        chunk = os.read(key.fd, 65536)
                        if not chunk:
                            selector.unregister(key.fileobj)
                            continue
                        need(key.data, "native_read_diagnostic")
                        yield chunk
                need(process.wait(timeout=max(0.01, deadline - time.monotonic())) == 0,
                     "native_read_failed")
        except (OSError, subprocess.TimeoutExpired):
            raise Unavailable("native_read_failed") from None
        finally:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
            process.stdout.close()
            process.stderr.close()

    def _read(self, argv, strict_diagnostics):
        try:
            process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE, start_new_session=True)
        except OSError:
            raise Unavailable("native_read_failed") from None
        deadline = time.monotonic() + self.timeout
        stdout = bytearray()
        diagnostic_seen = False
        size = 0
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ, True)
                selector.register(process.stderr, selectors.EVENT_READ, False)
                while selector.get_map():
                    remaining = deadline - time.monotonic()
                    need(remaining > 0, "native_read_timeout")
                    for key, _ in selector.select(min(remaining, 1)):
                        chunk = os.read(key.fd, 65536)
                        if not chunk:
                            selector.unregister(key.fileobj)
                            continue
                        size += len(chunk)
                        need(size <= MAX_BYTES, "native_read_size")
                        if key.data:
                            stdout.extend(chunk)
                        else:
                            diagnostic_seen = True
                need(process.wait(timeout=max(0.01, deadline - time.monotonic())) == 0, "native_read_failed")
            need(not (strict_diagnostics and diagnostic_seen), "native_read_diagnostic")
            return bytes(stdout)
        except (OSError, subprocess.TimeoutExpired):
            raise Unavailable("native_read_failed") from None
        finally:
            try:
                # A parent can exit while a descendant still owns its
                # pipe. Kill the group even when the parent has exited.
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
            process.stdout.close()
            process.stderr.close()


def main():
    # This executable is mounted at a fixed path by the no-argument verb.
    # Unexpected argv never turns it into a generic administrative reader.
    if len(sys.argv) != 1:
        raise Unavailable("unexpected_arguments")
    estate = decode(sys.stdin.buffer.read(MAX_BYTES + 1))
    with open("/target.json", "rb") as handle:
        target = decode(handle.read(MAX_BYTES + 1))
    return discover(estate, target, NativeReadPort())


def exit_code(report):
    if report["roster"]["state"] != "matched":
        return 4
    for node in report["nodes"]:
        if node["configuration"]["audit"] == "unknown" or node["capture_policy"]["state"] == "unavailable":
            return 4
        if node["capture_policy"]["coverage"]["state"] == "unavailable":
            return 4
    # Successfully measuring configuration never supplies a history pass.
    return 0


if __name__ == "__main__":
    try:
        report = main()
        print(json.dumps(report, sort_keys=True, separators=(",", ":"), allow_nan=False))
        sys.exit(exit_code(report))
    except Exception:
        # No traceback or native exception string crosses the operator boundary.
        print('{"schema":"boss.admission-source-discovery.v1","scope":"configuration_discovery_only","history_verdict":"unavailable","roster":{"state":"unavailable","reason":"operator_read_unavailable"},"nodes":[]}')
        sys.exit(4)
