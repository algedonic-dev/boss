"""The access probe derives protected paths inside the operator; no native reads."""
import importlib.util
import pathlib
import json
import subprocess
import ast
import unittest
import sys
sys.dont_write_bytecode = True
source = pathlib.Path(__file__).resolve().parents[1] / "admission-source.py"
spec = importlib.util.spec_from_file_location("discovery", source)
d = importlib.util.module_from_spec(spec)
spec.loader.exec_module(d)

class ProtectedSink(unittest.TestCase):
    def pod(self):
        return {"node":"192.0.2.11", "metadata":{"type":"StaticPods.kubernetes.talos.dev", "id":"kube-apiserver", "namespace":"k8s"}, "spec":{
            "apiVersion":"v1", "kind":"Pod", "metadata":{"name":"kube-apiserver"}, "spec":{
                "containers":[{"name":"kube-apiserver", "command":["kube-apiserver"], "args":["--audit-log-path=/logs/audit.log"],
                               "volumeMounts":[{"name":"audit", "mountPath":"/logs"}]}],
                "volumes":[{"name":"audit", "hostPath":{"path":"/var/log/apiserver", "type":"Directory"}}]}}}
    def test_path_comes_from_the_actual_volume_mapping(self):
        self.assertEqual(d.retained_sink(self.pod(), "192.0.2.11"), "/var/log/apiserver/audit.log")
    def test_foreign_node_unmounted_sink_and_traversal_refuse(self):
        for shape in ["foreign", "unmounted", "traversal", "duplicate", "subpath"]:
            with self.subTest(shape=shape):
                p = self.pod(); c = p["spec"]["spec"]["containers"][0]
                if shape == "foreign": p["node"]="192.0.2.99"
                if shape == "unmounted": c["volumeMounts"]=[]
                if shape == "traversal": c["args"]=["--audit-log-path=/logs/../credentials"]
                if shape == "duplicate": c["args"].append("--audit-log-path=/logs/another")
                if shape == "subpath": c["volumeMounts"][0]["subPath"]="unexpected"
                with self.assertRaises(d.Unavailable): d.retained_sink(p, "192.0.2.11")

class Entrypoint(unittest.TestCase):
    def test_native_memory_refusal_reaps_the_child_without_diagnostics(self):
        import os
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            pid_file = pathlib.Path(directory) / "pid"
            program = ("import os,resource,pathlib; "
                       "resource.setrlimit(resource.RLIMIT_AS,(50331648,50331648)); "
                       f"pathlib.Path({str(pid_file)!r}).write_text(str(os.getpid())); "
                       "data=b'x'*67108864")
            with self.assertRaises(d.Unavailable) as refusal:
                d.NativeReadPort().read_bytes([sys.executable, "-B", "-c", program])
            self.assertEqual(str(refusal.exception), "native_read_failed")
            pid = int(pid_file.read_text())
            with self.assertRaises(ProcessLookupError): os.kill(pid, 0)

    def test_missing_unlimited_or_excessive_native_limits_refuse(self):
        import tempfile
        entry_spec = importlib.util.spec_from_file_location("access_entry", source.with_name("probe-admission-source.py"))
        entry = importlib.util.module_from_spec(entry_spec); entry_spec.loader.exec_module(entry)
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            good = {"memory.max": "268435456\n", "memory.swap.max": "0\n", "pids.max": "64\n"}
            def write(values):
                for name, value in values.items(): (root / name).write_text(value)
            write(good)
            self.assertTrue(entry.native_limits_available(root))
            for name, value in [("memory.max", "max\n"), ("memory.max", "268435457\n"),
                                ("memory.swap.max", "1\n"), ("pids.max", "65\n"),
                                ("pids.max", "0\n"), ("pids.max", "x\n"),
                                ("memory.max", "1" * 65), ("pids.max", "1\n2\n")]:
                with self.subTest(name=name, value=value):
                    write(good); (root / name).write_text(value)
                    self.assertFalse(entry.native_limits_available(root))
            write(good); (root / "memory.swap.max").unlink()
            self.assertFalse(entry.native_limits_available(root))

    def test_arguments_and_malformed_input_cannot_expose_raw_diagnostics(self):
        for arguments,expected in [(["/unapproved/path"],"unexpected_arguments"),([],None)]:
            result=subprocess.run([sys.executable,"-B",str(source.with_name("probe-admission-source.py")),*arguments],
                input=b"PRIVATE-NATIVE-DIAGNOSTIC",capture_output=True)
            self.assertEqual(result.returncode,4)
            self.assertEqual(result.stderr,b"")
            self.assertNotIn(b"PRIVATE-NATIVE",result.stdout)
            reason = json.loads(result.stdout)["roster"]["reason"]
            if expected is not None: self.assertEqual(reason, expected)
            else:
                entry_spec = importlib.util.spec_from_file_location("access_entry", source.with_name("probe-admission-source.py"))
                entry = importlib.util.module_from_spec(entry_spec); entry_spec.loader.exec_module(entry)
                expected_reason = "operator_read_unavailable" if entry.native_limits_available(pathlib.Path("/sys/fs/cgroup")) else "operator_resource_unavailable"
                self.assertEqual(reason, expected_reason)

class ReceiptContract(unittest.TestCase):
    contract=source.with_name("admission-access-report.jq")
    def accepts(self, value):
        return subprocess.run(["jq","-e","-s","-f",str(self.contract)],input=json.dumps(value),text=True,capture_output=True).returncode==0
    def test_exact_producer_population_limits_pin_host_receipt_totals(self):
        import copy
        f,p=AccessProbe().fixtures();good=d.probe_access(f.ESTATE,f.TARGET,p)
        for field,limit in [("bytes",256*d.MAX_BYTES),("records",10000)]:
            budget=d.StreamBudget()
            if field == "bytes":
                budget.charge(encoded=limit)
                with self.assertRaises(d.Unavailable): budget.charge(encoded=1)
            else:
                budget.pass_records[0]=limit-1
                event=(json.dumps(RetainedRecords().event())+"\n").encode()
                class Port:
                    def stream_bytes(self,argv,deadline): yield event
                d.stream_member(Port(),[],False,budget,set())
                with self.assertRaises(d.Unavailable): d.stream_member(Port(),[],False,budget,set())
            report=copy.deepcopy(good)
            report["nodes"][0]["access"][field]=limit-1
            report["nodes"][1]["access"][field]=1
            self.assertTrue(self.accepts(report),field)
            report["nodes"][1]["access"][field]=2
            self.assertFalse(self.accepts(report),field+" global +1")
            report["nodes"][0]["access"][field]=limit+1
            self.assertFalse(self.accepts(report),field+" node +1")

    def test_actual_probe_reports_pass_the_host_contract(self):
        for flag in [None,"fail_file","unstable","changed_content","changed_roster"]:
            f,p=AccessProbe().fixtures()
            if flag:setattr(p,flag,True)
            self.assertTrue(self.accepts(d.probe_access(f.ESTATE,f.TARGET,p)),flag)
    def test_raw_fields_clean_verdict_bad_counts_and_freeform_reasons_refuse(self):
        import copy
        f,p=AccessProbe().fixtures();good=d.probe_access(f.ESTATE,f.TARGET,p)
        for mutate in [lambda r:r.update(history_verdict="clean"),lambda r:r.update(raw="private"),
                       lambda r:r["nodes"][0]["access"].update(raw="private"),
                       lambda r:r["nodes"][0]["access"].update(rotation_count=99),
                       lambda r:r["nodes"][0].update(access={"state":"unavailable","reason":"private_diagnostic"})]:
            bad=copy.deepcopy(good);mutate(bad);self.assertFalse(self.accepts(bad))
    def test_every_fixed_program_reason_has_a_machine_checked_contract_pin(self):
        reasons={"unsupported_shape","operator_read_unavailable","operator_resource_unavailable","unexpected_arguments"}
        for node in ast.walk(ast.parse(source.read_text())):
            if isinstance(node,ast.Call) and isinstance(node.func,ast.Name):
                index={"need":1,"Unavailable":0,"unknown":0}.get(node.func.id)
                if index is not None and len(node.args)>index and isinstance(node.args[index],ast.Constant) and isinstance(node.args[index].value,str):
                    reasons.add(node.args[index].value)
        for reason in sorted(reasons):
            report={"schema":"boss.admission-source-access.v1","scope":"retained_source_access_probe","history_verdict":"unavailable",
                    "roster":{"state":"unavailable","reason":reason},"nodes":[]}
            self.assertTrue(self.accepts(report),reason)

class AccessProbe(unittest.TestCase):
    def test_invalid_volume_graph_refuses_before_protected_reads(self):
        for shape in ["missing", "empty", "wrong_type", "duplicate", "dangling", "two_sources"]:
            with self.subTest(shape=shape):
                f, port = self.fixtures()
                for pod in port.pods.values():
                    spec = pod["spec"]["spec"]
                    mount = spec["containers"][0]["volumeMounts"][0]
                    volume = spec["volumes"][0]
                    if shape == "missing":
                        del mount["name"]; del volume["name"]
                    if shape == "empty": mount["name"] = volume["name"] = ""
                    if shape == "wrong_type": mount["name"] = volume["name"] = 7
                    if shape == "duplicate": spec["volumes"].append(dict(volume))
                    if shape == "dangling": mount["name"] = "absent"
                    if shape == "two_sources": volume["emptyDir"] = {}
                report = d.probe_access(f.ESTATE, f.TARGET, port)
                self.assertTrue(all(n["access"]["state"] == "unavailable" for n in report["nodes"]), report)
                self.assertEqual(port.byte_calls, [], "invalid mapping selected protected bytes")

    def test_native_container_has_fixed_heap_process_and_swap_bounds(self):
        shell = source.with_name("probe-admission-source-access.sh").read_text()
        for flag in ["--memory 256m", "--memory-swap 256m", "--pids-limit 64", "--rm"]:
            self.assertIn(flag, shell)

    def fixtures(self):
        fixture_spec=importlib.util.spec_from_file_location("source_fixtures",source.with_name("tests")/"admission-source-test.py")
        fixtures=importlib.util.module_from_spec(fixture_spec);fixture_spec.loader.exec_module(fixtures);fixtures.d=d
        payload=(json.dumps(RetainedRecords().event())+"\n").encode()
        class Port(fixtures.Port):
            def __init__(self):
                super().__init__();self.byte_calls=[];self.fail_file=False;self.unstable=False;self.changed_content=False;self.changed_roster=False;self.list_counts={};self.file_counts={}
                for ip in self.pods:
                    pod=ProtectedSink().pod();pod["node"]=ip
                    pod["spec"]["spec"]["containers"][0]["args"] += ["--audit-policy-file="+d.RENDERED_POLICY,
                       "--audit-log-maxage=30","--audit-log-maxbackup=10","--audit-log-maxsize=100"]
                    self.pods[ip]=pod
            def stream_bytes(self, argv, deadline):
                yield self.read_bytes(argv)
            def read_bytes(self, argv):
                self.byte_calls.append(argv)
                ip=argv[3]
                if argv[4]=="list":
                    if argv[5:] != ["--long","/var/log/apiserver"]:raise AssertionError("nonfixed inventory")
                    self.list_counts[ip]=self.list_counts.get(ip,0)+1
                    count=len(payload)+(1 if self.unstable and self.list_counts[ip]>1 else 0)
                    return (RetainedInventory.header+
                       RetainedInventory().row(".","drwx------",node=ip,size="4096")+
                       RetainedInventory().row("audit.log",node=ip,size=str(count))).encode()
                if argv[4:] != ["read","/var/log/apiserver/audit.log"]:raise AssertionError("nonfixed file")
                if self.fail_file and ip=="192.0.2.11":raise d.Unavailable("read_denied")
                self.file_counts[ip]=self.file_counts.get(ip,0)+1
                if self.changed_content and self.file_counts[ip]>1:
                    return payload.replace(b"private content",b"changed content")
                if self.changed_roster:
                    self.nodes["items"][0]["metadata"]["uid"]="changed-node"
                return payload
        return fixtures,Port()
    def test_real_fixed_native_method_reports_only_measured_access_not_clean_history(self):
        f,p=self.fixtures();report=d.probe_access(f.ESTATE,f.TARGET,p)
        self.assertEqual(report["roster"]["state"],"matched")
        self.assertEqual([n["access"]["state"] for n in report["nodes"]],["measured","measured"])
        self.assertEqual(report["history_verdict"],"unavailable")
        self.assertTrue(all(n["completeness"]["state"]=="unavailable" for n in report["nodes"]))
        self.assertEqual(len(p.byte_calls),10)
        for private in ["/var/log", "audit.log", "private identity", "private content", "private/request"]:
            self.assertNotIn(private,json.dumps(report))
    def test_denied_node_and_unstable_inventory_keep_every_contributor_unknown(self):
        for flag in ["fail_file","unstable"]:
            f,p=self.fixtures();setattr(p,flag,True);report=d.probe_access(f.ESTATE,f.TARGET,p)
            self.assertEqual(len(report["nodes"]),2)
            self.assertEqual(report["nodes"][0]["access"]["state"],"unavailable")
            self.assertEqual(report["history_verdict"],"unavailable")
    def test_same_size_content_change_and_roster_change_refuse_measured_access(self):
        for flag in ["changed_content","changed_roster"]:
            f,p=self.fixtures();setattr(p,flag,True);report=d.probe_access(f.ESTATE,f.TARGET,p)
            self.assertEqual(report["nodes"][0]["access"]["state"],"unavailable")
            self.assertEqual(report["nodes"][0]["access"]["reason"],"retained_source_unstable")
            if flag=="changed_roster":
                self.assertEqual(report["roster"]["state"],"unavailable")
                self.assertTrue(all(n["access"]["state"]=="unavailable" for n in report["nodes"]))

    def test_rotation_appearing_during_each_actual_second_full_read_refuses(self):
        f, port = self.fixtures()
        original = port.read_bytes
        changed = set()
        def read_bytes(argv):
            value = original(argv)
            if argv[4] == "list" and argv[3] in changed:
                value += RetainedInventory().row("audit-2026-10-05T12-00-00.000.log",
                                                  node=argv[3], size="17").encode()
            return value
        def stream_bytes(argv, deadline):
            value = read_bytes(argv)
            if port.file_counts[argv[3]] == 2:
                changed.add(argv[3])
            yield value
        port.read_bytes = read_bytes
        port.stream_bytes = stream_bytes
        report = d.probe_access(f.ESTATE, f.TARGET, port)
        self.assertEqual(len(changed), 2, "both actual second reads must reach the mutation")
        self.assertTrue(all(node["access"] == {"state": "unavailable", "reason": "retained_source_unstable"}
                            for node in report["nodes"]), report)
        self.assertTrue(ReceiptContract().accepts(report))

    def test_dark_roster_refuses_before_any_protected_file_read(self):
        f,p=self.fixtures();p.nodes["items"].pop(0)
        report=d.probe_access(f.ESTATE,f.TARGET,p)
        self.assertEqual(report["roster"]["state"],"unavailable")
        self.assertEqual(p.byte_calls,[])

class RetainedMembers(unittest.TestCase):
    def test_native_lumberjack_family_only_and_bounded_gzip(self):
        import gzip
        names=["audit.log","audit-2026-10-04T15-04-05.000.log.gz","unrelated.log"]
        rows=[{"name":name,"size":17} for name in names]
        got=d.retained_members(rows,"/var/log/apiserver/audit.log",10)
        self.assertEqual([r["name"] for r in got],names[:2])
        self.assertEqual(d.retained_payload(gzip.compress(b"bounded"),True),b"bounded")
    def test_corrupt_deflate_is_a_fixed_unavailable_not_an_unhandled_native_exception(self):
        broken=bytes.fromhex("1f8b08000000000000ff07")+b"\x00"*8
        with self.assertRaises(d.Unavailable) as error:d.retained_payload(broken,True)
        self.assertEqual(str(error.exception),"retained_compression_unavailable")

    def test_unknown_rotation_shape_missing_current_unbounded_and_gzip_bomb_refuse(self):
        import gzip
        for names,backups in [(["audit.log","audit.log.1"],10),(["audit-2026-10-04T15-04-05.000.log"],10),
                              (["audit.log"],0),(["audit.log","audit-2026-02-30T15-04-05.000.log"],10)]:
            with self.subTest(names=names),self.assertRaises(d.Unavailable):
                d.retained_members([{"name":n,"size":17} for n in names],"/var/log/apiserver/audit.log",backups)
        with self.assertRaises(d.Unavailable):d.retained_payload(gzip.compress(b"x"*1048577),True)
        with self.assertRaises(d.Unavailable):d.retained_payload(b"partial",True)

class RetainedSizeDiagnosis(unittest.TestCase):
    def test_numeric_inventory_bounds_name_the_cause(self):
        for size, reason in [(0, "retained_member_empty"), (1, None),
                             (d.MAX_BYTES, None), (d.MAX_BYTES + 1, "retained_member_size_bound")]:
            with self.subTest(size=size):
                listing = (RetainedInventory.header + RetainedInventory().row(".", "drwx------", size="4096")
                           + RetainedInventory().row("audit.log", size=str(size)))
                rows = d.retained_inventory(listing.encode(), "192.0.2.11")
                self.assertEqual(rows[0]["size"], size)
                if reason is None:
                    self.assertEqual(d.retained_members(rows, "/var/log/apiserver/audit.log", 10), rows)
                else:
                    with self.assertRaises(d.Unavailable) as error:
                        d.retained_members(rows, "/var/log/apiserver/audit.log", 10)
                    self.assertEqual(str(error.exception), reason)

    def test_member_refusal_precedes_any_file_read(self):
        for size, reason in [(0, "retained_member_empty"),
                             (32 * d.MAX_BYTES + 1, "retained_member_size_bound")]:
            with self.subTest(size=size):
                f, port = AccessProbe().fixtures()
                original = port.read_bytes
                def read_bytes(argv):
                    if argv[4] != "list":
                        return original(argv)
                    port.byte_calls.append(argv)
                    return (RetainedInventory.header
                            + RetainedInventory().row(".", "drwx------", node=argv[3], size="4096")
                            + RetainedInventory().row("audit.log", node=argv[3], size=str(size))).encode()
                port.read_bytes = read_bytes
                report = d.probe_access(f.ESTATE, f.TARGET, port)
                self.assertEqual(report["roster"]["state"], "matched")
                self.assertTrue(all(n["access"] == {"state":"unavailable", "reason":reason}
                                    for n in report["nodes"]))
                self.assertFalse(any(argv[4] == "read" for argv in port.byte_calls))
                self.assertEqual(report["history_verdict"], "unavailable")
                self.assertTrue(ReceiptContract().accepts(report))

    def test_gzip_decoded_bounds_are_distinct(self):
        import gzip
        for size, reason in [(0, "retained_decoded_empty"), (d.MAX_BYTES, None),
                             (d.MAX_BYTES + 1, "retained_decoded_size_bound")]:
            with self.subTest(size=size):
                raw = gzip.compress(b"x" * size)
                self.assertTrue(0 < len(raw) <= d.MAX_BYTES)
                if reason is None:
                    self.assertEqual(d.retained_payload(raw, True), b"x" * size)
                else:
                    with self.assertRaises(d.Unavailable) as error:
                        d.retained_payload(raw, True)
                    self.assertEqual(str(error.exception), reason)

    def test_decoded_rotation_refusal_reaches_the_sanitized_receipt(self):
        import gzip
        for size, reason in [(0, "retained_decoded_empty"),
                             (256 * 1024 + 1, "retained_stream_record_bound")]:
            with self.subTest(size=size):
                f, port = AccessProbe().fixtures()
                original = port.read_bytes
                rotation = "audit-2026-10-04T15-04-05.000.log.gz"
                raw = gzip.compress(b"x" * size)
                def read_bytes(argv):
                    if argv[4] == "list":
                        return original(argv) + RetainedInventory().row(rotation, node=argv[3], size=str(len(raw))).encode()
                    if argv[5].endswith("/" + rotation):
                        port.byte_calls.append(argv)
                        return raw
                    return original(argv)
                port.read_bytes = read_bytes
                report = d.probe_access(f.ESTATE, f.TARGET, port)
                self.assertTrue(all(n["access"] == {"state":"unavailable", "reason":reason}
                                    for n in report["nodes"]))
                self.assertEqual(report["history_verdict"], "unavailable")
                self.assertTrue(ReceiptContract().accepts(report))
                self.assertTrue(all(set(n["access"]) == {"state", "reason"} for n in report["nodes"]))
                for private in [rotation, "/var/log", "raw", "private content"]:
                    self.assertNotIn(private, json.dumps(report))

    def test_unknown_stat_is_not_an_empty_member(self):
        for size in ["unknown", "", "1MiB", "-1", "1.0"]:
            with self.subTest(size=size):
                listing = (RetainedInventory.header + RetainedInventory().row(".", "drwx------", size="4096")
                           + RetainedInventory().row("audit.log", size=size))
                with self.assertRaises(d.Unavailable) as error:
                    d.retained_inventory(listing.encode(), "192.0.2.11")
                self.assertEqual(str(error.exception), "retained_inventory_shape")

    def test_historical_reason_stays_accepted_without_becoming_clean(self):
        f, port = AccessProbe().fixtures()
        report = d.probe_access(f.ESTATE, f.TARGET, port)
        self.assertTrue(all(n["access"]["state"] == "measured" for n in report["nodes"]))
        self.assertEqual(report["history_verdict"], "unavailable")
        for node in report["nodes"]:
            node["access"] = {"state":"unavailable", "reason":"retained_file_size"}
        self.assertTrue(ReceiptContract().accepts(report))
        report["history_verdict"] = "clean"
        self.assertFalse(ReceiptContract().accepts(report))

class RetainedRecords(unittest.TestCase):
    def event(self):
        return {"apiVersion":"audit.k8s.io/v1", "kind":"Event", "level":"Metadata", "stage":"ResponseComplete",
                "auditID":"00000000-0000-4000-8000-000000000001", "requestReceivedTimestamp":"2026-10-04T15:04:05.123456789Z",
                "stageTimestamp":"2026-10-04T15:04:06.123456789Z", "user":{"username":"private identity"},
                "requestURI":"/private/request", "annotations":{"private":"private content"}}
    def test_exact_nanosecond_bounds_count_and_opaque_content_identity_only(self):
        raw=(json.dumps(self.event())+"\n").encode()
        got=d.retained_records(raw)
        self.assertEqual(got["records"],1)
        self.assertEqual(got["first_retained"],"2026-10-04T15:04:05.123456789Z")
        self.assertEqual(got["last_retained"],"2026-10-04T15:04:06.123456789Z")
        self.assertEqual(set(got),{"records","first_retained","last_retained","identity"})
        self.assertNotIn("private",json.dumps(got))
    def test_empty_partial_duplicate_key_bad_identity_timestamp_and_reversed_bounds_refuse(self):
        values=[b"",json.dumps(self.event()).encode(), b'{"kind":"Event","kind":"Event"}\n']
        for key,value in [("apiVersion","v1"),("auditID","private"),("stageTimestamp","2026-10-04T15:04:05.123456788Z"),
                          ("stageTimestamp","2026-02-30T15:04:06Z"),("stage","unknown")]:
            event=self.event();event[key]=value;values.append((json.dumps(event)+"\n").encode())
        for raw in values:
            with self.subTest(raw=raw),self.assertRaises(d.Unavailable):d.retained_records(raw)
    def test_duplicate_event_stage_or_missing_contributor_never_counts_twice(self):
        raw=(json.dumps(self.event())+"\n").encode()
        with self.assertRaises(d.Unavailable):d.retained_records(raw+raw)

class RetainedInventory(unittest.TestCase):
    header = "NODE   MODE   UID   GID   SIZE(B)   LASTMOD   LABEL   NAME\n"
    def row(self, name, mode="-rw-------", node="192.0.2.11", size="17"):
        return f"{node}   {mode}   0   0   {size}   Oct  4 15:04:05      {name}\n"
    def listing(self):
        return self.header + self.row(".", "drwx------", size="4096") + self.row("audit.log")
    def test_actual_long_format_preserves_the_complete_private_inventory(self):
        rows = d.retained_inventory(self.listing().encode(), "192.0.2.11")
        self.assertEqual(rows, [{"name":"audit.log", "size":17, "mode":"-rw-------", "uid":0, "gid":0,
                                 "modified_display":"Oct  4 15:04:05", "label":""}])
    def test_foreign_node_duplicate_partial_header_path_link_and_overflow_refuse(self):
        for raw in [self.listing()+self.row("foreign", node="192.0.2.99"),
                    self.listing()+self.row("audit.log"), self.listing().split("\n",1)[1],
                    self.listing()+self.row("../credentials"),
                    self.listing()+self.row("audit.log -> private", mode="Lrwx------"),
                    self.listing()+self.row("unbounded", size="9223372036854775808"),
                    self.header+self.row("audit.log")]:
            with self.subTest(raw=raw), self.assertRaises(d.Unavailable):
                d.retained_inventory(raw.encode(), "192.0.2.11")
    def test_invalid_calendar_or_clock_and_unknown_column_shape_refuse(self):
        for change in ["Oct 99 15:04:05", "Oct  4 99:04:05", "Feb 30 2025 15:04"]:
            with self.subTest(change=change), self.assertRaises(d.Unavailable):
                d.retained_inventory(self.listing().replace("Oct  4 15:04:05", change).encode(), "192.0.2.11")
    def test_actual_nonempty_selinux_label_column_is_preserved(self):
        raw=self.listing().replace("      audit.log", "   system_u:object_r:var_log_t:s0   audit.log")
        self.assertEqual(d.retained_inventory(raw.encode(), "192.0.2.11")[0]["label"], "system_u:object_r:var_log_t:s0")

    def test_modified_display_is_preserved_and_not_claimed_as_an_exact_timestamp(self):
        raw=self.listing().replace("Oct  4 15:04:05", "Oct  4 2025 15:04")
        self.assertEqual(d.retained_inventory(raw.encode(), "192.0.2.11")[0]["modified_display"], "Oct  4 2025 15:04")

class ProtectedBytes(unittest.TestCase):
    def command(self, program):
        return [sys.executable, "-B", "-c", program]
    def test_file_bytes_are_not_decoded_or_written_to_a_file(self):
        raw = b'{"kind":"Event"}\n'
        self.assertEqual(d.NativeReadPort().read_bytes(self.command(f'import sys; sys.stdout.buffer.write({raw!r})')), raw)
    def test_success_with_diagnostic_output_is_unavailable(self):
        with self.assertRaises(d.Unavailable) as error:
            d.NativeReadPort().read_bytes(self.command("import sys; print('partial'); print('protected diagnostic', file=sys.stderr)"))
        self.assertEqual(str(error.exception), "native_read_diagnostic")
        self.assertNotIn("protected", str(error.exception))
    def test_overflow_and_timeout_refuse_with_fixed_reasons(self):
        for program, timeout, reason in [
                ("import sys; sys.stdout.buffer.write(b'x' * 1048577)", 25, "native_read_size"),
                ("import time; time.sleep(5)", 0.05, "native_read_timeout")]:
            with self.subTest(reason=reason), self.assertRaises(d.Unavailable) as error:
                d.NativeReadPort(timeout=timeout).read_bytes(self.command(program))
            self.assertEqual(str(error.exception), reason)
    def test_nonzero_exit_never_returns_partial_source(self):
        with self.assertRaises(d.Unavailable) as error:
            d.NativeReadPort().read_bytes(self.command("import sys; print('partial'); sys.exit(7)"))
        self.assertEqual(str(error.exception), "native_read_failed")

if __name__ == "__main__": unittest.main()
