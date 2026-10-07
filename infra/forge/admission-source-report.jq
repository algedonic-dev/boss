# Host receipt contract. Protected native bytes never reach this filter;
# nevertheless, an unexpected operator answer is refused, never echoed.
def keys_are($expected): (type == "object") and ((keys | sort) == ($expected | sort));
def hash: type == "string" and test("^[a-f0-9]{64}$");
def reason: . as $r | [
    "unsupported_shape", "source_size", "duplicate_key", "nonfinite_json", "source_shape", "invalid_json",
    "estate_shape", "estate_identity", "estate_retirement", "estate_address", "estate_control_plane_count",
    "native_roster_shape", "native_roster_incomplete", "native_roster_identity", "native_roster_address", "control_plane_roster_mismatch", "control_plane_address_mismatch",
    "target_shape", "target_stages", "named_resource_coverage_unavailable", "native_resource_identity", "native_resource_shape",
    "native_pod_shape", "native_pod_command", "audit_argument_shape", "duplicate_audit_argument", "audit_retention_shape",
    "retained_source_access_not_established", "effective_default_not_measured", "effective_policy_not_established",
    "native_policy_shape", "native_read_timeout", "native_read_size", "native_read_failed", "read_denied", "not_measured",
    "audit_group_wildcard_version_unavailable"
] | index($r) != null;
def unavailable: keys_are(["state","reason"]) and .state == "unavailable" and (.reason | reason);
def bound: unavailable or (keys_are(["state","value"]) and .state == "configured" and
    (.value | type == "number" and . >= 0 and . <= 999999999 and floor == .));
def configuration:
    (keys_are(["provenance","audit","reason"]) and .provenance == "talos_generated_static_pod"
        and .audit == "unknown" and (.reason | reason)) or
    (keys_are(["provenance","audit","sinks","retention","policy_file_identity"])
        and .provenance == "talos_generated_static_pod"
        and (.audit == "configured_enabled" or .audit == "configured_disabled")
        and (.policy_file_identity == null or (.policy_file_identity | hash))
        and (.sinks | type == "array" and length <= 2 and all(.[];
            keys_are(["class","identity"]) and (.identity | hash)
            and (.class == "file" or .class == "stdout" or .class == "webhook_configuration")))
        and (.retention | keys_are(["max_age_days","max_backups","max_size_megabytes"]) and all(.[]; bound)));
def coverage: unavailable or (keys_are(["state","request_count","covered_count","target_identity","stage_count"])
    and (.state == "covered" or .state == "not_covered") and (.target_identity | hash)
    and (.request_count | type == "number" and . > 0 and . <= 32 and floor == .)
    and (.covered_count | type == "number" and . >= 0 and floor == .)
    and .covered_count <= .request_count
    and (.stage_count | type == "number" and . > 0 and . <= 4 and floor == .)
    and (.state != "covered" or .covered_count == .request_count));
def capture_policy:
    (keys_are(["state","reason"]) and unavailable) or
    (keys_are(["state","reason","coverage"]) and .state == "unavailable" and (.reason | reason) and (.coverage | unavailable)) or
    (keys_are(["state","identity","coverage"]) and .state == "measured_configuration" and (.identity | hash) and (.coverage | coverage));
def node:
    keys_are(["node","native_identity","configuration","capture_policy","runtime_capture","first_retained","last_retained","rotations","drops","errors","completeness"])
    and (.node | type == "string" and test("^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$"))
    and (.native_identity | hash) and (.configuration | configuration) and (.capture_policy | capture_policy)
    and all(.runtime_capture,.first_retained,.last_retained,.rotations,.drops,.errors,.completeness; unavailable);
length == 1 and (.[0] |
    keys_are(["history_verdict","nodes","roster","schema","scope"])
    and .schema == "boss.admission-source-discovery.v1" and .scope == "configuration_discovery_only"
    and .history_verdict == "unavailable" and (.nodes | type == "array" and length <= 16 and all(.[]; node))
    and (if .roster.state == "unavailable" then (.roster | unavailable) and (.nodes | length == 0)
         else (.roster | keys_are(["state","registered_count","native_count","identity"]) and .state == "matched" and (.identity | hash))
            and .roster.registered_count == (.nodes | length) and .roster.native_count == (.nodes | length)
            and (.nodes | length > 0) and ([.nodes[].node] | length == (unique | length)) end))
