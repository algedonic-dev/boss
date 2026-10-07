# The host accepts only the sanitized access receipt. Never a history pass.
# A program-reason parity test pins this independent allowlist.
def keys_are($expected): type == "object" and ((keys|sort)==($expected|sort));
def hash: type == "string" and test("^[a-f0-9]{64}$");
def integer($low;$high): type == "number" and floor == . and . >= $low and . <= $high;
def reason: . as $r | ["audit_argument_shape", "audit_group_wildcard_version_unavailable", "audit_retention_shape", "control_plane_address_mismatch", "control_plane_roster_mismatch", "duplicate_audit_argument", "duplicate_key", "effective_default_not_measured", "effective_policy_not_established", "estate_address", "estate_control_plane_count", "estate_http_unavailable", "estate_identity", "estate_retirement", "estate_shape", "estate_unavailable", "historical_continuity_unavailable", "invalid_json", "named_resource_coverage_unavailable", "native_pod_command", "native_pod_shape", "native_policy_shape", "native_read_diagnostic", "native_read_failed", "native_read_size", "native_read_timeout", "native_resource_identity", "native_resource_shape", "native_roster_address", "native_roster_identity", "native_roster_incomplete", "native_roster_shape", "nonfinite_json", "not_measured", "read_denied", "operator_configuration_unavailable", "operator_image_unavailable", "operator_read_unavailable", "operator_resource_unavailable", "operator_report_unavailable", "operator_tool_unavailable", "retained_compression_unavailable", "retained_file_size", "retained_member_empty", "retained_member_size_bound", "retained_decoded_empty", "retained_decoded_size_bound", "retained_inventory_identity", "retained_inventory_incomplete", "retained_inventory_shape", "retained_inventory_size", "retained_inventory_type", "retained_population_size", "retained_record_count", "retained_record_duplicate", "retained_record_identity", "retained_record_shape", "retained_record_timestamp", "retained_records_incomplete", "retained_rotation_bound", "retained_rotation_duplicate", "retained_rotation_incomplete", "retained_rotation_shape", "retained_sink_unavailable", "retained_source_access_not_established", "retained_source_unstable", "source_drop_coverage_unavailable", "source_error_coverage_unavailable", "source_shape", "source_size", "target_shape", "target_stages", "unexpected_arguments", "unsupported_shape"] | index($r) != null;
def stream_reason: . as $r | ["retained_stream_chunk_shape", "retained_stream_time_bound", "retained_stream_work_bound", "retained_stream_decoded_work_bound", "retained_stream_decoded_population_bound", "retained_stream_decoded_member_bound", "retained_stream_record_bound", "retained_stream_member_bound"] | index($r) != null;
def unavailable: keys_are(["state","reason"]) and .state == "unavailable" and (.reason|(reason or stream_reason));
def timestamp: type == "string" and test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?Z$");
def access: unavailable or (
    keys_are(["state","file_count","rotation_count","bytes","records","first_retained","last_retained","inventory_identity","content_identity","continuity"])
    and .state == "measured" and .continuity == "stable_two_reads_only"
    and (.file_count|integer(1;17)) and (.rotation_count|integer(0;16)) and .rotation_count == .file_count - 1
    and (.bytes|integer(1;268435456)) and (.records|integer(1;10000))
    and (.first_retained|timestamp) and (.last_retained|timestamp)
    and (.inventory_identity|hash) and (.content_identity|hash));
def node:
    keys_are(["node","native_identity","access","sink_identity","drops","errors","completeness"])
    and (.node|type == "string" and test("^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$"))
    and (.native_identity|hash) and (.sink_identity == null or (.sink_identity|hash))
    and (.access|access) and (.access.state != "measured" or (.sink_identity|hash))
    and .drops == {"state":"unavailable","reason":"source_drop_coverage_unavailable"}
    and .errors == {"state":"unavailable","reason":"source_error_coverage_unavailable"}
    and .completeness == {"state":"unavailable","reason":"historical_continuity_unavailable"};
length == 1 and (.[0]|
    keys_are(["schema","scope","history_verdict","roster","nodes"])
    and .schema == "boss.admission-source-access.v1" and .scope == "retained_source_access_probe"
    and .history_verdict == "unavailable"
    and (.nodes|type == "array" and length <= 16 and all(.[];node))
    and ([.nodes[].node]|length == (unique|length))
    and ([.nodes[] | select(.access.state == "measured") | .access.bytes] | (add // 0) | integer(0;268435456))
    and ([.nodes[] | select(.access.state == "measured") | .access.records] | (add // 0) | integer(0;10000))
    and (if .roster.state == "unavailable" then (.roster|unavailable) and all(.nodes[];.access.state == "unavailable")
         else (.roster|keys_are(["state","registered_count","native_count","identity"]) and .state == "matched" and (.identity|hash)
               and (.registered_count|integer(1;16)) and .registered_count == .native_count)
               and .roster.registered_count == (.nodes|length) end))
