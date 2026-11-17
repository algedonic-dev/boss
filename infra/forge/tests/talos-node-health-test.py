"""The real shell/ops_talosctl boundary, without a cluster or credentials."""
import json
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[3]
ETCD_CASES = {"etcd-members": ["etcd", "members"], "etcd-status": ["etcd", "status"],
              "etcd-alarms": ["etcd", "alarm", "list"]}


def collector():
    spec = importlib.util.spec_from_file_location("talos_health", ROOT / "infra/forge/talos-node-health.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class HealthDoor(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix=f"talos-health-{os.getuid()}-{os.getpid()}-")
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name)
        (self.path / "bin").mkdir()
        (self.path / "nodes.json").write_text(json.dumps({"data": [{
            "id": "cp-1", "address": "192.0.2.10", "role": "talos-control-plane", "retired": False
        }]}))
        (self.path / "node.json").write_text(json.dumps({"apiVersion": "v1", "kind": "Node",
            "metadata": {"name": "cp-1", "uid": "fixture-node", "labels": {"node-role.kubernetes.io/control-plane": ""}},
            "status": {"addresses": [{"type": "InternalIP", "address": "192.0.2.10"}]}}))
        stub = self.path / "bin/sudo"
        stub.write_text('''#!/usr/bin/env python3
import json, os, sys
a=sys.argv[1:]
if a[:4]==['-n','docker','image','inspect']: sys.exit(0)
if '--kubeconfig=/kc' in a:
 a=a[a.index('--kubeconfig=/kc')+1:]
 with open(os.environ['KUBE_CALLS'],'a') as f: f.write(json.dumps(a)+'\\n')
 if a!=['get','node','cp-1','-o','json','--request-timeout=20s']: sys.exit(94)
 print(open(os.environ['NODE_JSON']).read()); sys.exit(0)
if '--talosconfig=/tc' not in a: sys.exit(91)
# No host file or shell reaches the container, and no mutation is accepted.
a=a[a.index('--talosconfig=/tc')+1:]
with open(os.environ['CALLS'],'a') as f: f.write(json.dumps(a)+'\\n')
if a[:2]!=['-n','192.0.2.10']: sys.exit(92)
a=a[2:]
allowed=[['service'], *[['service',s,'status'] for s in ['machined','kubelet','etcd','containerd','apid','trustd']],
 ['dmesg'], *[['read',p] for p in ['/proc/pressure/memory','/proc/pressure/io','/proc/pressure/cpu','/proc/meminfo','/proc/uptime','/proc/loadavg']],
 *[['logs',s,'--tail','200'] for s in ['machined','kubelet','etcd']],
 *json.loads(os.environ['ETCD_ARGV'])]
if a not in allowed: sys.exit(93)
mode=os.environ.get('MODE','ok')
if mode=='flood':
 while True: os.write(1,b'x'*4096)
if mode=='bad':
 print('rpc unavailable',file=sys.stderr); sys.exit(7)
if a[0]=='etcd':
 if a==['etcd','members']:
  print('NODE ID HOSTNAME PEER URLS CLIENT URLS LEARNER')
  print('cp-1 1 cp-1 https://192.0.2.10:2380 https://192.0.2.10:2379 false')
 elif a==['etcd','status']:
  print('NODE MEMBER DB SIZE IN USE LEADER RAFT INDEX RAFT TERM RAFT APPLIED INDEX LEARNER PROTOCOL STORAGE ERRORS')
  print('cp-1 1 12 MB 9 MB (75.00%) 1 123 7 123 false 3.6 3.6')
 elif mode!='empty':
  print('MEMBER ALARM')
  print('1 NOSPACE')
 if mode=='warning': print('WARNING: partial response from member',file=sys.stderr)
 if mode=='redacted': print('password=password')
 sys.exit(0)
print('HEALTH retained event')
print('password=password')
print('serial: fixture-serial',file=sys.stderr)
print('-----BEGIN PRIVATE KEY-----\\nfixture-key\\n-----END PRIVATE KEY-----')
print('safe stderr',file=sys.stderr)
''')
        stub.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.path / "bin") + ":" + os.environ["PATH"],
                        BOSS_ESTATE_NODES_URL=(self.path / "nodes.json").as_uri(),
                        BOSS_FORGE_REGISTRY_HOST="reg.test", BOSS_OPS_DIR=str(self.path / "no-credentials"),
                        BOSS_TALOSCTL=str(self.path / "talosctl"), CALLS=str(self.path / "calls"),
                        KUBE_CALLS=str(self.path / "kube-calls"), NODE_JSON=str(self.path / "node.json"),
                        ETCD_ARGV=json.dumps(list(ETCD_CASES.values())))
        self.env.pop("BOSS_JOBS_URL", None)

    def run_door(self, *args, mode="ok"):
        script = os.environ.get("HEALTH_DOOR", "infra/forge/talos-node-health.sh")
        return subprocess.run(["bash", str(ROOT / script), *args], env=dict(self.env, MODE=mode),
                              capture_output=True, timeout=35)

    def test_fixed_readings_cross_the_actual_container_argv_boundary(self):
        cases = [("services",), ("service", "machined"), ("service", "kubelet"),
                 ("service", "etcd"), ("service", "containerd"), ("service", "apid"),
                 ("service", "trustd"), ("dmesg",), ("pressure-memory",), ("pressure-io",),
                 ("pressure-cpu",), ("meminfo",), ("uptime",), ("loadavg",),
                 ("logs", "kubelet"), ("logs", "etcd"), ("logs", "machined")]
        for args in cases:
            with self.subTest(args=args):
                out = self.run_door("cp-1", *args)
                self.assertEqual(out.returncode, 0, (out.stdout, out.stderr))
                self.assertIn(b"HEALTH retained event", out.stdout)
                self.assertIn(b"safe stderr", out.stderr)
                for secret in [b"password=password", b"fixture-serial", b"fixture-key"]:
                    self.assertNotIn(secret, out.stdout + out.stderr)
        self.assertEqual(len((self.path / "calls").read_text().splitlines()) if (self.path / "calls").exists() else 0, len(cases))

    def test_refusals_never_open_the_talos_door(self):
        for args in [("cp-1", "read", "/etc/secrets"), ("cp-1", "service", "restart"),
                     ("cp-1", "logs", "apid"), ("cp-1", "uptime", "extra"),
                     ("cp-1,cp-2", "services"), ("unknown", "services")]:
            out = self.run_door(*args)
            self.assertEqual(out.returncode, 78, (args, out.stdout, out.stderr))
        self.assertFalse((self.path / "calls").exists())

    def test_etcd_fixed_readings_cross_the_real_control_plane_door(self):
        for reading, command in ETCD_CASES.items():
            with self.subTest(reading=reading):
                out = self.run_door("cp-1", reading)
                self.assertEqual(out.returncode, 0, (out.stdout, out.stderr))
                calls = [json.loads(line) for line in (self.path / "calls").read_text().splitlines()]
                self.assertEqual(calls[-1], ["-n", "192.0.2.10", *command])
                self.assertTrue((self.path / "kube-calls").exists(), "control-plane identity was never read")
                self.assertIn(b"cp-1" if reading != "etcd-alarms" else b"NOSPACE", out.stdout)

    def test_etcd_worker_and_registry_refusals_keep_the_target_unopened(self):
        original = json.loads((self.path / "nodes.json").read_text())
        for kind in ("worker", "retired", "duplicate", "address-list", "malformed", "dark"):
            with self.subTest(kind=kind):
                rows = json.loads(json.dumps(original))
                if kind == "worker": rows['data'][0]['role'] = 'talos-worker'
                elif kind == "retired": rows['data'][0]['retired'] = True
                elif kind == "duplicate": rows['data'].append(rows['data'][0].copy())
                elif kind == "address-list": rows['data'][0]['address'] += ',192.0.2.11'
                (self.path / "nodes.json").write_text('not-json' if kind == 'malformed' else json.dumps(rows))
                if kind == 'dark': (self.path / "nodes.json").unlink()
                out = self.run_door('cp-1', 'etcd-status')
                self.assertIn(out.returncode, (1, 78), (out.stdout, out.stderr))
                self.assertFalse((self.path / "calls").exists())
                self.assertFalse((self.path / "kube-calls").exists())
        (self.path / "nodes.json").write_text(json.dumps(original))
        original['data'][0]['role'] = 'talos-worker'
        (self.path / "nodes.json").write_text(json.dumps(original))
        out = self.run_door('cp-1', 'services')
        self.assertEqual(out.returncode, 0, (out.stdout, out.stderr))

    def test_etcd_actual_node_role_and_address_must_agree(self):
        original = json.loads((self.path / 'node.json').read_text())
        for kind in ('worker-label', 'address'):
            with self.subTest(kind=kind):
                node = json.loads(json.dumps(original))
                if kind == 'worker-label': node['metadata']['labels'] = {}
                else: node['status']['addresses'][0]['address'] = '192.0.2.11'
                (self.path / 'node.json').write_text(json.dumps(node))
                out = self.run_door('cp-1', 'etcd-members')
                self.assertEqual(out.returncode, 78, (out.stdout, out.stderr))
                self.assertTrue((self.path / 'kube-calls').exists(), 'actual identity guard never ran')
                self.assertFalse((self.path / 'calls').exists())

    def test_etcd_mutation_words_and_extra_services_are_not_commands(self):
        for args in [('etcd-status', 'etcd'), ('etcd-members', 'remove-member'),
                     ('etcd', 'defrag'), ('etcd-alarms', 'disarm'), ('snapshot',), ('compact',)]:
            with self.subTest(args=args):
                out = self.run_door('cp-1', *args)
                self.assertEqual(out.returncode, 78, (out.stdout, out.stderr))
                self.assertFalse((self.path / 'calls').exists())

    def test_etcd_partial_warning_and_withheld_rows_are_unknown(self):
        for mode in ('warning', 'redacted'):
            with self.subTest(mode=mode):
                out = self.run_door('cp-1', 'etcd-status', mode=mode)
                self.assertNotEqual(out.returncode, 0, (out.stdout, out.stderr))
                self.assertIn(b'12 MB', out.stdout)
                self.assertIn(b'completeness=UNKNOWN', out.stderr)
                self.assertNotIn(b'password=password', out.stdout + out.stderr)
        out = self.run_door('cp-1', 'etcd-status', mode='flood')
        self.assertNotEqual(out.returncode, 0)
        self.assertIn(b'byte limit', out.stderr)
        self.assertIn(b'completeness=UNKNOWN', out.stderr)

    def test_etcd_empty_alarm_output_is_not_a_zero_alarm_health_claim(self):
        out = self.run_door('cp-1', 'etcd-alarms', mode='empty')
        self.assertEqual(out.returncode, 0, (out.stdout, out.stderr))
        self.assertIn(b'alarm_set=UNKNOWN', out.stdout)
        self.assertIn(b'health=UNKNOWN', out.stdout)
        self.assertIn(b'single-node observation', out.stdout)

    def test_failed_and_oversize_reads_are_not_clean_health(self):
        bad = self.run_door("cp-1", "services", mode="bad")
        self.assertNotEqual(bad.returncode, 0)
        self.assertIn(b"rpc unavailable", bad.stderr)
        flood = self.run_door("cp-1", "dmesg", mode="flood")
        self.assertNotEqual(flood.returncode, 0)
        self.assertIn(b"byte limit", flood.stderr)
        self.assertLess(len(flood.stdout) + len(flood.stderr), 550000)

    def test_fixed_rosters_equal_the_request_descriptor(self):
        module = collector()
        verb = json.loads((ROOT / "infra/ops/verbs/talos-node-health.json").read_text())
        self.assertEqual(verb["params"][1]["one_of"],
                         ["services", "service", "dmesg", *module.READS, "logs", *ETCD_CASES])
        for reading, command in ETCD_CASES.items():
            self.assertEqual(module.argv(reading, ''), command)
        self.assertEqual(verb["params"][2]["one_of"], list(module.SERVICES))
        self.assertNotIn("effect", verb)
        self.assertEqual(verb["hosts"], ["forge"])
        self.assertEqual(verb.get("capture"), "separate-streams")

    def test_timed_out_child_is_reaped_and_partial_channels_survive(self):
        module = collector()
        buffers, status, reason = module.collect(
            ["/bin/sh", "-c", "printf 'started\\n'; printf 'safe-error\\n' >&2; exec sleep 60"], seconds=2)
        self.assertEqual(reason, "time limit")
        self.assertLess(status, 0)
        self.assertIn(b"started", buffers[1])
        self.assertIn(b"safe-error", buffers[2])

    def test_tail_and_redaction_are_explicit_on_both_channels(self):
        module = collector()
        raw = b"-----BEGIN PRIVATE KEY-----\nfixture-key\n-----END PRIVATE KEY-----\n"
        raw += b"safe line\n" * 300 + b"last retained\npassword=password\nserial: fixture-serial\n"
        lines, count, withheld = module.safe_lines(raw)
        self.assertEqual(len(lines), module.LINES)
        self.assertGreater(count, len(lines))
        self.assertIn("last retained", lines)
        self.assertGreaterEqual(withheld, 5)
        self.assertNotIn("fixture-serial", "\n".join(lines))


if __name__ == "__main__":
    unittest.main()
