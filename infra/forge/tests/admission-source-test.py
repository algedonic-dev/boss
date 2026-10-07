"""Exercise the actual operator sanitizer against hostile native fixtures; no cluster."""
import copy
import importlib.util
import json
import pathlib
import sys
import unittest

sys.dont_write_bytecode = True

SOURCE = pathlib.Path(__file__).resolve().parents[1] / "admission-source.py"
spec = importlib.util.spec_from_file_location("discovery", SOURCE)
d = importlib.util.module_from_spec(spec)
spec.loader.exec_module(d)

ESTATE = {"data": [{"id": "cp-1", "role": "talos-control-plane", "address": "192.0.2.11", "retired": False},
                   {"id": "cp-2", "role": "talos-control-plane", "address": "192.0.2.12", "retired": False},
                   {"id": "w-1", "role": "talos-worker", "address": "192.0.2.21", "retired": False}]}
NODES = {"apiVersion": "v1", "kind": "NodeList", "items": [
    {"apiVersion": "v1", "kind": "Node", "metadata": {"name": r["id"], "uid": "uid-" + r["id"], "labels": ({"node-role.kubernetes.io/control-plane": ""} if r["role"] == "talos-control-plane" else {})},
     "status": {"addresses": [{"type": "InternalIP", "address": r["address"]}]}} for r in ESTATE["data"]]}
TARGET = {"policy": "example-policy", "binding": "example-binding", "namespace": "example", "user": "system:serviceaccount:example:actor", "groups": ["system:serviceaccounts", "system:serviceaccounts:example", "system:authenticated"],
          "requests": [{"group": "apps", "resource": "deployments", "verb": "patch"}, {"group": "apps", "resource": "statefulsets", "verb": "patch"}], "stages": ["ResponseComplete"]}
POLICY = {"apiVersion": "audit.k8s.io/v1", "kind": "Policy", "rules": [{"level": "Metadata"}]}

def native(ip, kind, ident, body):
    return {"node": ip, "metadata": {"type": kind, "id": ident, "namespace": "k8s" if kind.startswith("StaticPods.") else "controlplane"}, "spec": body}

def pod(ip, args=None):
    return native(ip, "StaticPods.kubernetes.talos.dev", "kube-apiserver", {"apiVersion": "v1", "kind": "Pod", "metadata": {"name": "kube-apiserver"},
         "spec": {"containers": [{"name": "kube-apiserver", "command": ["kube-apiserver"], "args": args or ["--audit-policy-file=/system/config/kubernetes/kube-apiserver/auditpolicy.yaml", "--audit-log-path=/private/log", "--audit-log-maxage=7", "--audit-log-maxbackup=3", "--audit-log-maxsize=20"], "env": [{"name": "TOKEN", "value": "DO-NOT-EXPORT"}]}]}})

class Port:
    def __init__(self):
        self.calls = []
        self.nodes = copy.deepcopy(NODES)
        self.pods = {r["address"]: pod(r["address"]) for r in ESTATE["data"][:2]}
        self.policies = {r["address"]: native(r["address"], "AuditPolicyConfigs.kubernetes.talos.dev", "audit-policy", {"config": copy.deepcopy(POLICY)}) for r in ESTATE["data"][:2]}
        self.fail = set()
    def read(self, argv):
        self.calls.append(argv)
        if tuple(argv) in self.fail:
            raise d.Unavailable("read_denied")
        if argv == ["kubectl", "--kubeconfig=/kc", "get", "nodes", "-o", "json", "--request-timeout=20s"]:
            return copy.deepcopy(self.nodes)
        ip = argv[3]
        if argv[5] == "StaticPods.kubernetes.talos.dev":
            return copy.deepcopy(self.pods[ip])
        if argv[5] == "AuditPolicyConfigs.kubernetes.talos.dev":
            return copy.deepcopy(self.policies[ip])
        raise AssertionError("nonfixed read")

class Discovery(unittest.TestCase):
    def test_kubectl_v133_list_preserves_every_control_plane(self):
        # Measured with kubectl v1.33.3 get nodes -o json against a local
        # credential-free API serving NodeList: the printer emits v1 List.
        for kind in ["List", "NodeList"]:
            with self.subTest(kind=kind):
                p = Port(); p.nodes.update(kind=kind, metadata={"resourceVersion": ""})
                report = d.discover(ESTATE, TARGET, p)
                self.assertEqual(report["roster"]["state"], "matched")
                self.assertEqual(report["roster"]["registered_count"], 2)
                self.assertEqual(report["roster"]["native_count"], 2)
                self.assertEqual([n["node"] for n in report["nodes"]], ["cp-1", "cp-2"])
                self.assertEqual(len(p.calls), 5)
                self.assertEqual(report["history_verdict"], "unavailable")
    def test_wrong_list_or_any_item_kind_version_refuses_before_protected_reads(self):
        mutations = [lambda n: n.update(kind="PodList"),
                     lambda n: n.update(apiVersion="apps/v1"),
                     lambda n: n.pop("apiVersion"),
                     lambda n: n["items"][0].update(kind="Pod"),
                     lambda n: n["items"][0].update(apiVersion="v2"),
                     lambda n: n["items"][0].pop("kind"),
                     lambda n: n["items"][2].update(kind="Pod"),
                     lambda n: n["items"][2].pop("apiVersion")]
        for kind in ["List", "NodeList"]:
            for index, mutate in enumerate(mutations):
                with self.subTest(kind=kind, mutation=index):
                    p = Port(); p.nodes["kind"] = kind; mutate(p.nodes)
                    report = d.discover(ESTATE, TARGET, p)
                    self.assertEqual(report["roster"]["state"], "unavailable")
                    self.assertEqual(report["nodes"], [])
                    self.assertEqual(len(p.calls), 1)
    def test_kubectl_list_keeps_completeness_and_identity_refusals(self):
        mutations = [lambda n: n.update(metadata={"continue":"next"}),
                     lambda n: n.update(metadata={"remainingItemCount":1}),
                     lambda n: n["items"].pop(0),
                     lambda n: n["items"].append(copy.deepcopy(n["items"][0])),
                     lambda n: n["items"][0]["status"]["addresses"][0].update(address="192.0.2.99"),
                     lambda n: n["items"][0]["metadata"].pop("uid"),
                     lambda n: n["items"][2]["metadata"]["labels"].update({"node-role.kubernetes.io/control-plane":""}),
                     lambda n: n.update(items=n["items"] * 86)]
        for index, mutate in enumerate(mutations):
            with self.subTest(mutation=index):
                p = Port(); p.nodes["kind"] = "List"; mutate(p.nodes)
                report = d.discover(ESTATE, TARGET, p)
                self.assertEqual(report["roster"]["state"], "unavailable")
                self.assertEqual(report["nodes"], [])
                self.assertEqual(len(p.calls), 1)
    def test_real_sanitizer_reports_match_the_receipt_contract(self):
        import subprocess
        fixture_ports = [Port(), Port(), Port(), Port(), Port()]
        fixture_ports[1].pods["192.0.2.11"] = {}
        fixture_ports[2].policies["192.0.2.11"]["spec"]["config"] = {}
        fixture_ports[3].nodes["items"].pop(0)
        fixture_ports[4].nodes["metadata"] = {"continue": "opaque-next-page"}
        contract = SOURCE.with_name("admission-source-report.jq")
        for port in fixture_ports:
            report = d.discover(ESTATE, TARGET, port)
            output = subprocess.run(["jq", "-e", "-s", "-f", str(contract)], input=json.dumps(report), text=True, capture_output=True)
            self.assertEqual(output.returncode, 0, output.stderr + json.dumps(report))
            for mutate in [lambda r: r.update(raw="DO-NOT-EXPORT"), lambda r: r["roster"].update(raw="DO-NOT-EXPORT")]:
                hostile = copy.deepcopy(report); mutate(hostile)
                output = subprocess.run(["jq", "-e", "-s", "-f", str(contract)], input=json.dumps(hostile), text=True, capture_output=True)
                self.assertNotEqual(output.returncode, 0)
    def test_malformed_one_resource_keeps_all_nodes_unknown_without_throwing(self):
        p = Port(); p.pods["192.0.2.11"]["spec"]["metadata"] = None
        r = d.discover(ESTATE, TARGET, p)
        self.assertEqual(len(r["nodes"]), 2)
        self.assertEqual(r["nodes"][0]["configuration"]["audit"], "unknown")
        self.assertEqual(r["nodes"][1]["configuration"]["audit"], "configured_enabled")
    def test_complete_roster_and_only_fixed_reads(self):
        p = Port(); p.nodes["metadata"] = {"continue": "", "remainingItemCount": 0}
        r = d.discover(ESTATE, TARGET, p)
        self.assertEqual(r["roster"]["state"], "matched")
        self.assertEqual([n["node"] for n in r["nodes"]], ["cp-1", "cp-2"])
        self.assertEqual(len(p.calls), 5)
        for call in p.calls[1:]:
            self.assertEqual(call[:2], ["/talosctl", "--talosconfig=/tc"])
            self.assertIn(call[3], ["192.0.2.11", "192.0.2.12"])
            self.assertEqual(call[-2:], ["-o", "json"])
    def test_configuration_is_not_capture_or_retained_history(self):
        r = d.discover(ESTATE, TARGET, Port())
        for n in r["nodes"]:
            self.assertEqual(n["configuration"]["provenance"], "talos_generated_static_pod")
            self.assertEqual(n["configuration"]["audit"], "configured_enabled")
            self.assertEqual(n["capture_policy"]["coverage"]["state"], "covered")
            for field in ["runtime_capture", "first_retained", "last_retained", "rotations", "drops", "errors", "completeness"]:
                self.assertEqual(n[field]["state"], "unavailable", field)
        self.assertEqual(r["history_verdict"], "unavailable")
    def test_secrets_paths_and_raw_policy_never_emitted(self):
        p = Port(); p.policies["192.0.2.11"]["spec"]["config"]["rules"][0]["users"] = ["DO-NOT-EXPORT"]
        text = json.dumps(d.discover(ESTATE, TARGET, p))
        for forbidden in ["DO-NOT-EXPORT", "/private/", "system:serviceaccount", "containers", "rules", "env"]:
            self.assertNotIn(forbidden, text)
    def test_roster_mismatch_never_reads_protected_resources(self):
        for mutate in [lambda n: n["items"].pop(0),
                       lambda n: n["items"][0]["status"]["addresses"][0].update(address="192.0.2.99"),
                       lambda n: n["items"][2]["metadata"]["labels"].update({"node-role.kubernetes.io/control-plane": ""}),
                       lambda n: n["items"].append(copy.deepcopy(n["items"][0]))]:
            p = Port(); mutate(p.nodes); r = d.discover(ESTATE, TARGET, p)
            self.assertEqual(r["roster"]["state"], "unavailable")
            self.assertEqual(len(p.calls), 1)
            self.assertEqual(r["nodes"], [])
    def test_continued_native_node_list_is_unavailable_before_talos_reads(self):
        for metadata in [{"continue": "opaque-next-page"}, {"remainingItemCount": 1}, {"remainingItemCount": "0"}, {"remainingItemCount": False}, {"continue": None}, None]:
            with self.subTest(metadata=metadata):
                p = Port(); p.nodes["metadata"] = metadata
                report = d.discover(ESTATE, TARGET, p)
                self.assertEqual(report["roster"]["state"], "unavailable")
                self.assertEqual(report["nodes"], [])
                self.assertEqual(len(p.calls), 1)
    def test_retired_duplicate_invalid_address_refused_before_native_read(self):
        for mutate in [lambda e: e["data"][0].update(retired=True), lambda e: e["data"].append(copy.deepcopy(e["data"][0])), lambda e: e["data"][0].update(address="999.0.2.11"), lambda e: e.update(data=[])]:
            p = Port(); e = copy.deepcopy(ESTATE); mutate(e); r = d.discover(e, TARGET, p)
            self.assertEqual(r["roster"]["state"], "unavailable")
            self.assertLessEqual(len(p.calls), 1)
            self.assertEqual(r["nodes"], [])
    def test_dark_one_node_preserves_all_contributors_and_unknown(self):
        p = Port(); p.pods["192.0.2.11"] = {}
        r = d.discover(ESTATE, TARGET, p)
        self.assertEqual(len(r["nodes"]), 2)
        self.assertEqual(r["nodes"][0]["configuration"]["audit"], "unknown")
        self.assertEqual(r["nodes"][1]["configuration"]["audit"], "configured_enabled")
        self.assertEqual(d.exit_code(r), 4)
        self.assertEqual(d.exit_code(d.discover(ESTATE, TARGET, Port())), 0)
    def test_wrong_native_identity_or_duplicate_arg_is_unknown(self):
        for mutate in [lambda o: o.update(node="192.0.2.99"), lambda o: o["metadata"].update(id="other"),
                       lambda o: o["metadata"].pop("namespace"),
                       lambda o: o["spec"]["spec"]["containers"][0]["args"].append("--audit-log-path=/secret/other")]:
            p = Port(); mutate(p.pods["192.0.2.11"])
            self.assertEqual(d.discover(ESTATE, TARGET, p)["nodes"][0]["configuration"]["audit"], "unknown")
    def test_first_match_none_and_omitted_stage_are_not_covered(self):
        for policy in [{"apiVersion": "audit.k8s.io/v1", "kind": "Policy", "rules": [{"level": "None", "users": [TARGET["user"]]}, {"level": "Metadata"}]},
                       {"apiVersion": "audit.k8s.io/v1", "kind": "Policy", "omitStages": ["ResponseComplete"], "rules": [{"level": "Metadata"}]}]:
            p = Port(); p.policies["192.0.2.11"]["spec"]["config"] = policy
            self.assertEqual(d.discover(ESTATE, TARGET, p)["nodes"][0]["capture_policy"]["coverage"]["state"], "not_covered")
    def test_audit_matching_is_not_rbac_wildcard_matching(self):
        policies = [
            ([{"level": "Metadata", "verbs": ["*"]}], "not_covered"),
            ([{"level": "Metadata", "resources": [{"group": "*", "resources": ["*"]}]}], "unavailable"),
            ([{"level": "None", "resources": [{"group": "apps", "resources": ["deployments/*"]}]}, {"level": "Metadata"}], "not_covered"),
        ]
        for rules, expected in policies:
            with self.subTest(rules=rules):
                p = Port(); p.policies["192.0.2.11"]["spec"]["config"]["rules"] = rules
                self.assertEqual(d.discover(ESTATE, TARGET, p)["nodes"][0]["capture_policy"]["coverage"]["state"], expected, rules)
    def test_unsupported_or_malformed_policy_is_unavailable(self):
        for policy in [{"apiVersion": "audit.k8s.io/v1", "kind": "Policy", "rules": [{"level": "invented"}]},
                       {"apiVersion": "audit.k8s.io/v1", "kind": "Policy", "rules": [{"level": "Metadata", "resources": [{"group": "apps", "resources": ["deployments"], "resourceNames": ["specific"]}]}]},
                       {"kind": "Policy", "rules": []}]:
            p = Port(); p.policies["192.0.2.11"]["spec"]["config"] = policy
            self.assertEqual(d.discover(ESTATE, TARGET, p)["nodes"][0]["capture_policy"]["coverage"]["state"], "unavailable")
    def test_generated_command_flags_are_measured_and_alternate_policy_unavailable(self):
        p = Port(); c = p.pods["192.0.2.11"]["spec"]["spec"]["containers"][0]
        c["command"] = ["/usr/local/bin/kube-apiserver"] + c.pop("args")
        self.assertEqual(d.discover(ESTATE, TARGET, p)["nodes"][0]["capture_policy"]["coverage"]["state"], "covered")
        c["command"][1] = "--audit-policy-file=/different/policy"
        self.assertEqual(d.discover(ESTATE, TARGET, p)["nodes"][0]["capture_policy"]["coverage"]["state"], "unavailable")
    def test_native_read_port_bounds_and_suppresses_native_error_bytes(self):
        import sys
        port = d.NativeReadPort()
        for program in ["import sys; print('DO-NOT-EXPORT',file=sys.stderr);sys.exit(1)",
                        "print('x' * (1024 * 1024 + 1))", "print('{}{}')"]:
            with self.assertRaises(d.Unavailable) as caught:
                port.read([sys.executable, "-c", program])
            self.assertNotIn("DO-NOT-EXPORT", str(caught.exception))
        self.assertEqual(port.read([sys.executable, "-c", "print('{\"native\":true}')"]), {"native": True})
        import time
        started = time.monotonic()
        with self.assertRaises(d.Unavailable) as caught:
            d.NativeReadPort(timeout=0.1).read([sys.executable, "-c", "import subprocess,sys; subprocess.Popen([sys.executable,'-c','import time;time.sleep(5)']);sys.exit(0)"])
        self.assertEqual(str(caught.exception), "native_read_timeout")
        self.assertLess(time.monotonic() - started, 1)

    def test_strict_json_rejects_duplicates_multiple_docs_and_nonfinite(self):
        for raw in [b'{"x":1,"x":2}', b'{}{}', b'{"x":NaN}', b'', b'[]']:
            with self.assertRaises(d.Unavailable):
                d.decode(raw)
    def test_declared_resource_filter_covers_both_and_not_a_partial(self):
        p = Port(); p.policies["192.0.2.11"]["spec"]["config"]["rules"] = [{"level": "Metadata", "users": [TARGET["user"]], "verbs": ["patch"], "namespaces": ["example"], "resources": [{"group": "apps", "resources": ["deployments", "statefulsets"]}]}]
        self.assertEqual(d.discover(ESTATE, TARGET, p)["nodes"][0]["capture_policy"]["coverage"]["state"], "covered")
        p.policies["192.0.2.11"]["spec"]["config"]["rules"][0]["resources"][0]["resources"].pop()
        self.assertEqual(d.discover(ESTATE, TARGET, p)["nodes"][0]["capture_policy"]["coverage"]["state"], "not_covered")

if __name__ == "__main__":
    unittest.main(verbosity=2)
