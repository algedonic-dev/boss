#!/usr/bin/env python3
"""The native reader is exercised through its bounded command port only."""
import copy
import hashlib
import importlib.util
import json
import pathlib
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

HERE = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('gate_storage', HERE.parent / 'gate-storage.py')
storage = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = storage
spec.loader.exec_module(storage)
TALOS = ['sudo', '-n', 'docker', 'run', '--rm', '--log-driver=none', '--network', 'host',
         '--mount', 'type=bind,src=/synthetic/talosconfig,dst=/tc,readonly',
         '--mount', 'type=bind,src=/synthetic/talosctl,dst=/talosctl,readonly',
         'synthetic-ops-image', 'timeout', '20', '/talosctl', '--talosconfig=/tc']


def assert_public_refusal(case, script, arrival_delay=0):
    """Only private sentinels are reachable even if argument checking regresses."""
    with tempfile.TemporaryDirectory(prefix='gate-storage-refusal-') as folder:
        root = pathlib.Path(folder)
        for name in ('forge', 'estate'):
            (root / name).mkdir()
        calls = root / 'forbidden-calls'
        wrapper = root / 'forge/read-gate-storage.sh'
        wrapper.write_text(script)
        (root / 'estate/ops-credentials.sh').write_text(
            'printf "sourced\\n" >> "$REFUSAL_CALLS"\n'
            'ops_image_ready() { printf "image\\n" >> "$REFUSAL_CALLS"; }\n'
            'ops_kubectl() { printf "kubectl\\n" >> "$REFUSAL_CALLS"; echo false; }\n'
            'ops_talosctl() { printf "talos\\n" >> "$REFUSAL_CALLS"; echo false; }\n')
        (root / 'forge/gate-storage.py').write_text(
            'import os,pathlib\npathlib.Path(os.environ["REFUSAL_CALLS"]).write_text("child\\n")\n')
        # Delay only process arrival, not a production deadline or native read.
        launch = root / 'launch.py'
        launch.write_text('import os,sys,time\ntime.sleep(float(sys.argv[1]))\n'
                          'os.execv(sys.argv[2],sys.argv[2:])\n')
        result = subprocess.run(
            [sys.executable, str(launch), str(arrival_delay), shutil.which('bash'),
             '--noprofile', '--norc', str(wrapper), '../arbitrary'],
            env={'PATH': os.defpath, 'LANG': 'C.UTF-8', 'REFUSAL_CALLS': str(calls)},
            # Diagnostic hang watchdog, not a two-second startup contract.
            # Safety is the exact refusal and the absence of helper effects.
            capture_output=True, timeout=30)
        case.assertEqual(result.returncode, 78, result.stderr)
        case.assertIn(b'no arguments', result.stderr)
        case.assertFalse(calls.exists(), calls.read_text() if calls.exists() else '')


class Contract(unittest.TestCase):
    def test_global_bounds_replay_the_originals_even_when_cached_state_changes(self):
        class OverflowFixture(NativeFixture):
            def execute(self, argv, timeout):
                result = super().execute(argv, timeout)
                if argv[-2:] == ['read', '/sys/class/nvme/nvme1/state'] and not getattr(self, 'overflowed', False):
                    self.overflowed = True
                    return -9, b'x' * storage.SCALAR_BYTES, b'', False
                return result
        port = OverflowFixture()
        bounded = storage.Collector(storage.Recorder(port.execute, clock=lambda: port.now),
                                    ['kubectl'], TALOS, 'registry-helper', sleep=port.sleep).collect()
        self.assertEqual(bounded['state'], 'unavailable')
        original = copy.deepcopy(bounded['readings'])
        bounded['state'] = 'observed'
        self.assertEqual(storage.derive(bounded)['state'], 'unavailable')
        self.assertEqual(bounded['readings'], original)

    def test_global_bounds_cover_the_whole_roster_without_rejecting_complete_field_refusals(self):
        control = example_record()
        for fault in ('not-attempted', 'incomplete', 'call-time', 'whole-time', 'scalar-bytes', 'total-bytes', 'roster'):
            bad = copy.deepcopy(control)
            row = next(r for r in bad['readings'] if r['operation'] == 'sample0:/sys/class/nvme/nvme1/state')
            if fault == 'not-attempted':
                row.update(state='not-attempted', reason='observation bound reached')
                for key in ('stdout', 'stderr', 'exit', 'stream_complete'):
                    row.pop(key, None)
            elif fault == 'incomplete':
                row.update(state='unavailable', exit=-9, stream_complete=False)
            elif fault == 'call-time':
                row['end_monotonic'] = row['begin_monotonic'] + storage.CALL_SECONDS + 1
            elif fault == 'whole-time':
                row = bad['readings'][-1]
                row['begin_monotonic'], row['end_monotonic'] = 200, 201
            elif fault == 'scalar-bytes':
                row['stdout'] = storage.byte_record(b'x' * (storage.SCALAR_BYTES + 1))
            elif fault == 'total-bytes':
                for r in bad['readings']:
                    r['stderr'] = storage.byte_record(b'x' * 100000)
            else:
                for index in range(storage.MAX_READS):
                    extra = copy.deepcopy(row)
                    extra['operation'] = 'extra-' + str(index)
                    bad['readings'].append(extra)
            with self.subTest(fault=fault):
                self.assertEqual(storage.derive(bad)['state'], 'unavailable')
        complete_refusal = copy.deepcopy(control)
        row = next(r for r in complete_refusal['readings'] if r['operation'] == 'sample0:/sys/class/nvme/nvme1/state')
        row.update(state='unavailable', exit=23, stream_complete=True,
                   stdout=storage.byte_record(b''), stderr=storage.byte_record(b'actual complete field refusal\n'))
        result = storage.derive(complete_refusal)
        self.assertEqual(result['state'], 'observed')
        self.assertEqual(result['inventory']['non_success'], 1)

    def test_selected_client_hash_reads_actual_public_fixture_bytes(self):
        with tempfile.TemporaryDirectory(prefix='gate-storage-client-') as folder:
            path = pathlib.Path(folder) / 'selected-client'
            original = b'public synthetic executable bytes\x00\xff\n'
            path.write_bytes(original)
            recorder = storage.Recorder()
            row = recorder.call('before.client-sha256', ['sha256sum', '--', str(path)])
            self.assertEqual(storage.client_hash(row, str(path)), hashlib.sha256(original).hexdigest())
            with self.assertRaises(ValueError):
                storage.client_hash(row, str(path) + '-different')
            path.unlink()
            missing = recorder.call('after.client-sha256', ['sha256sum', '--', str(path)])
            self.assertEqual(missing['state'], 'unavailable')
            with self.assertRaises(ValueError):
                storage.client_hash(missing, str(path))

    def test_replay_reconstructs_native_identity_and_cohort_instead_of_trusting_caches(self):
        port = NativeFixture()
        record = storage.Collector(storage.Recorder(port.execute, clock=lambda: port.now),
                                   ['kubectl'], TALOS, 'registry-helper', sleep=port.sleep).collect()
        self.assertEqual(storage.derive(record)['state'], 'observed')
        for fault in ('missing-originals', 'cached-partition', 'cached-cohort', 'raw-device', 'raw-target', 'missing-optional-sample'):
            bad = copy.deepcopy(record)
            if fault == 'missing-originals':
                bad['readings'] = [r for r in bad['readings'] if not r['operation'].startswith(('before.', 'after.'))]
            elif fault == 'cached-partition':
                for key in ('identity_before', 'identity_after'):
                    bad[key]['device']['whole'] = 'nvme1n1p1'
            elif fault == 'cached-cohort':
                bad['cohort_before'] = bad['cohort_after'] = []
            elif fault == 'raw-device':
                row = next(r for r in bad['readings'] if r['operation'] == 'before.volume')
                value = storage.one_json(storage.text(row))
                value['spec']['parentLocation'] = '/dev/nvme0n1'
                row['stdout'] = storage.byte_record(json.dumps(value).encode())
            elif fault == 'raw-target':
                row = next(r for r in bad['readings'] if r['operation'] == 'sample0:/proc/diskstats')
                row['argv'][-3] = '192.0.2.99'
            else:
                bad['readings'] = [r for r in bad['readings'] if r['operation'] != 'sample0:/sys/class/nvme/nvme1/state']
            with self.subTest(fault=fault):
                self.assertEqual(storage.derive(bad)['state'], 'unavailable')
        for operation in (r['operation'] for r in record['readings'] if r['operation'].startswith(('before.', 'after.'))):
            bad = copy.deepcopy(record)
            bad['readings'] = [r for r in bad['readings'] if r['operation'] != operation]
            with self.subTest(missing=operation):
                self.assertEqual(storage.derive(bad)['state'], 'unavailable')

    def test_actual_client_hash_is_required_before_interpretation(self):
        port = NativeFixture()
        record = storage.Collector(storage.Recorder(port.execute, clock=lambda: port.now),
                                   ['kubectl'], TALOS, 'registry-helper', sleep=port.sleep).collect()
        result = storage.derive(record)
        self.assertEqual(result['state'], 'observed')
        self.assertIn('client_sha256', result)
        self.assertEqual(result['client_sha256'], 'a' * 64)
        for fault in ('missing', 'unavailable', 'wrong-path', 'malformed', 'changed'):
            bad = copy.deepcopy(record)
            row = next(r for r in bad['readings'] if r['operation'] == 'after.client-sha256')
            if fault == 'missing':
                bad['readings'].remove(row)
            elif fault == 'unavailable':
                row['state'], row['exit'] = 'unavailable', 23
            elif fault == 'wrong-path':
                row['argv'][-1] = '/synthetic/another-client'
            else:
                value = 'not a hash\n' if fault == 'malformed' else 'b' * 64 + '  /synthetic/talosctl\n'
                row['stdout'] = storage.byte_record(value.encode())
            with self.subTest(fault=fault):
                self.assertEqual(storage.derive(bad)['state'], 'unavailable')

    def test_success_requires_native_exit_complete_streams_and_ordered_clock(self):
        row = {'operation': 'synthetic', 'state': 'read', 'exit': 0,
               'stream_complete': True, 'stdout': storage.byte_record(b'ok'),
               'begin_monotonic': 1.0, 'end_monotonic': 2.0}
        self.assertEqual(storage.text(row), 'ok')
        for field, value in (('exit', 23), ('stream_complete', False),
                             ('begin_monotonic', float('nan')),
                             ('end_monotonic', 0.0)):
            with self.subTest(field=field):
                with self.assertRaises(ValueError):
                    storage.text({**row, field: value})

    def test_public_shell_refuses_arguments_before_any_native_door(self):
        assert_public_refusal(self, (HERE.parent / 'read-gate-storage.sh').read_text())

    def test_public_shell_argument_refusal_survives_delayed_process_arrival(self):
        assert_public_refusal(self, (HERE.parent / 'read-gate-storage.sh').read_text(), arrival_delay=2.2)

    def test_argument_refusal_control_detects_sourcing_before_the_guard(self):
        script = (HERE.parent / 'read-gate-storage.sh').read_text()
        guard = script[script.index('if [ "$#"'):script.index('\nHERE=')]
        source_line = '. "$HERE/../estate/ops-credentials.sh"'
        mutant = script.replace(guard, '', 1).replace(source_line, source_line + '\n' + guard, 1)
        with self.assertRaisesRegex(AssertionError, 'sourced'):
            assert_public_refusal(self, mutant)

    def test_no_caller_arguments_reach_the_registered_door(self):
        verb = json.loads((HERE.parents[1] / 'ops/verbs/read-gate-storage.json').read_text())
        self.assertEqual(verb['params'], [])
        self.assertEqual(verb['argv'], ['infra/forge/read-gate-storage.sh'])
        self.assertEqual(verb['hosts'], ['forge'])
        self.assertEqual(verb['capture'], 'separate-streams')

    def test_identity_requires_bound_pvc_pv_and_single_worker(self):
        pvc = {'metadata': {'uid':'pvc-1','name':'gate','namespace':'boss-dev'}, 'spec':{'volumeName':'gate-w-1'}, 'status':{'phase':'Bound'}}
        pv = {'metadata':{'uid':'pv-1','name':'gate-w-1'}, 'spec':{'claimRef':{'uid':'pvc-1','name':'gate','namespace':'boss-dev'}, 'local':{'path':'/var/mnt/gate'}, 'nodeAffinity':{'required':{'nodeSelectorTerms':[{'matchExpressions':[{'key':'kubernetes.io/hostname','operator':'In','values':['w-1']}]}]}}}}
        self.assertEqual(storage.topology(pvc,pv)['node'],'w-1')
        for changed in ('claim', 'path', 'affinity'):
            bad=copy.deepcopy(pv)
            if changed=='claim': bad['spec']['claimRef']['uid']='another'
            if changed=='path': bad['spec']['local']['path']='/var'
            if changed=='affinity': bad['spec']['nodeAffinity']['required']['nodeSelectorTerms'].append(copy.deepcopy(bad['spec']['nodeAffinity']['required']['nodeSelectorTerms'][0]))
            with self.assertRaises(ValueError): storage.topology(pvc,bad)

    def test_actual_talos_json_stream_binds_volume_to_native_disk(self):
        volume={'node':'10.0.0.1','metadata':{'id':'u-gate','type':'VolumeStatuses.block.talos.dev'},'spec':{'phase':'ready','filesystem':'xfs','location':'/dev/nvme1n1p1','parentLocation':'/dev/nvme1n1','partitionUUID':'volume-id','mountSpec':{'targetPath':'gate','parentId':'/var/mnt'}}}
        disk={'node':'10.0.0.1','metadata':{'id':'nvme1n1','type':'Disks.block.talos.dev'},'spec':{'dev_path':'/dev/nvme1n1','wwid':'eui.synthetic','serial':'synthetic','model':'synthetic NVMe'}}
        text=json.dumps(disk)+'\n'+json.dumps({**disk,'metadata':{'id':'nvme0n1'},'spec':{'dev_path':'/dev/nvme0n1'}})+'\n'
        result=storage.device(storage.json_stream(json.dumps(volume)),storage.json_stream(text),'10.0.0.1')
        self.assertEqual(result['whole'],'nvme1n1')
        self.assertEqual(result['partition'],'nvme1n1p1')
        for field in ('parentLocation', 'location'):
            bad = copy.deepcopy(volume)
            bad['spec'][field] = bad['spec'][field].removeprefix('/dev/')
            with self.assertRaises(ValueError):
                storage.device([bad], [disk], '10.0.0.1')
        for original in (volume, disk):
            bad = copy.deepcopy(original)
            bad['metadata']['type'] = 'MachineConfigs.config.talos.dev'
            with self.assertRaises(ValueError):
                storage.device([bad if original is volume else volume],
                               [bad if original is disk else disk], '10.0.0.1')
        for field,value in (('node','another'),('spec',{})):
            bad=copy.deepcopy(volume); bad[field]=value
            with self.assertRaises(ValueError): storage.device([bad],[disk],'10.0.0.1')

    def test_read_plan_never_accepts_an_arbitrary_path_or_resource(self):
        for path in ('/etc/shadow','/proc/1/environ','/proc/self/cmdline','/sys/fs/cgroup/../../etc/passwd','/dev/nvme1n1','/sys/class/nvme/nvme1/reset_controller'):
            with self.assertRaises(ValueError): storage.read_argv(path,{'whole':'nvme1n1','controller':'nvme1'},set())
        self.assertEqual(storage.read_argv('/proc/diskstats',{},set()),['read','/proc/diskstats'])
        with self.assertRaises(ValueError): storage.read_argv('/etc/io.stat',{}, {'/etc'})

    def test_journal_conserves_binary_and_trailing_newlines(self):
        row=storage.byte_record(b'\xff\x00\n\n')
        self.assertEqual(storage.decode_record(row),b'\xff\x00\n\n')
        for key,value in (('bytes',0),('sha256','0'*64),('base64','bad!')):
            bad={**row,key:value}
            with self.assertRaises(ValueError): storage.decode_record(bad)

    def test_deltas_use_whole_device_and_reject_counter_reset(self):
        a='259 5 nvme1n1 10 0 100 50 20 0 200 70 0 20 100 0 0 0 0\n259 6 nvme1n1p1 10 0 100 50 20 0 200 70 7 20 100 0 0 0 0\n'
        b='259 5 nvme1n1 12 0 104 60 23 0 206 100 0 40 150 0 0 0 0\n'
        delta=storage.disk_delta(a,b,'nvme1n1',2)
        self.assertEqual(delta['read_await_ms'],5)
        self.assertEqual(delta['write_await_ms'],10)
        self.assertEqual(delta['write_bytes'],3072)
        for text,seconds in ((a.replace('10 0 100','1 0 10'),2),(b,0),(b,True)):
            with self.assertRaises(ValueError): storage.disk_delta(a,text,'nvme1n1',seconds)
        with self.assertRaises(ValueError): storage.disk_delta(a,b.replace('259 5','259 7'),'nvme1n1',2)

    def test_cgroup_missing_whole_device_is_unavailable_not_zero(self):
        with self.assertRaises(ValueError): storage.io_delta('259:6 rbytes=1 wbytes=1 rios=1 wios=1\n','259:6 rbytes=2 wbytes=2 rios=2 wios=2\n','259:5')
        delta=storage.io_delta('259:5 rbytes=1 wbytes=2 rios=1 wios=1\n','259:5 rbytes=2 wbytes=8 rios=2 wios=2\n','259:5')
        self.assertEqual(delta['accounted_write_bytes'],6)
        self.assertNotIn('submitted_bytes',delta)

    def test_budget_records_unattempted_operations_without_running_them(self):
        clock=[0.0]
        calls=[]
        runner=storage.Recorder(lambda argv,timeout: (calls.append(argv) or (0,b'ok\n',b'',True)),clock=lambda:clock[0],wall=lambda:'synthetic',limit=1)
        runner.call('one',['safe'])
        with self.assertRaises(ValueError):
            runner.call('two',['never'])
        self.assertEqual(len(calls),1)
        self.assertEqual(len(runner.rows),1)
        runner.limit=3
        clock[0]=181
        runner.call('three',['never'])
        self.assertEqual(len(calls),1)
        self.assertEqual(runner.rows[1]['state'],'not-attempted')

    def test_native_error_words_never_become_proven_absence(self):
        r=storage.Recorder(lambda argv,timeout:(1,b'',b'NotFound permission denied unsupported',True))
        row=r.call('reading',['fixed'])
        self.assertEqual(row['state'],'unavailable')
        self.assertEqual(storage.decode_record(row['stderr']),b'NotFound permission denied unsupported')

    def test_cgroup_derivation_is_exact_uid_and_fixed_basename_only(self):
        uid='12345678-1234-4234-8234-123456789012'
        paths=storage.cgroup_paths(uid)
        self.assertTrue(all(uid in p or uid.replace('-','_') in p for p in paths))
        with self.assertRaises(ValueError): storage.cgroup_paths('../../etc')
        self.assertTrue(all('/environ' not in p for p in paths))

    def test_actual_pinned_talos_list_flushes_header_after_names(self):
        calls=[]
        def execute(argv,timeout):
            calls.append(argv)
            if 'list' in argv: return 0,b'.\nhwmon3\nNODE   NAME\n',b'',True
            return 0,b'nvme\n',b'',True
        collector=storage.Collector(storage.Recorder(execute),['kubectl'],['talosctl'],'registry')
        collector.address='192.0.2.1'; collector.bound={'controller':'nvme1'}
        collector.discover_hwmon()
        self.assertEqual(collector.hwmon,'hwmon3')
        self.assertEqual(calls[0],['talosctl','-n','192.0.2.1','list','/sys/class/nvme/nvme1/device/hwmon','--depth=1','--type=d'])
        self.assertEqual(calls[1][-2:],['read','/sys/class/nvme/nvme1/device/hwmon/hwmon3/name'])

    def test_oversized_and_timed_out_real_children_do_not_pass(self):
        # Positive output controls allow ordinary process arrival; the short
        # deadline below deliberately tests cancellation, not byte transport.
        result=storage.native_command([sys.executable,'-c',"print('x' * 200000)"],storage.CALL_SECONDS)
        self.assertFalse(result[3])
        self.assertEqual(result[1],b'x' * storage.SCALAR_BYTES)
        self.assertEqual(result[2],b'')
        result=storage.native_command([sys.executable,'-c','import time; time.sleep(5)'],0.05)
        self.assertFalse(result[3])
        # Cancellation is the child's termination, not a scheduling deadline
        # on its observing parent. The gate harness bounds a hung control.
        self.assertEqual(result[0], -signal.SIGKILL)
        result=storage.native_command([sys.executable,'-c',"import sys;sys.stdout.buffer.write(b'\\xff\\x00\\n');sys.stderr.write('native-error\\n');sys.exit(23)"],storage.CALL_SECONDS)
        self.assertEqual(result,(23,b'\xff\x00\n',b'native-error\n',True))

    def test_real_child_output_controls_survive_delayed_process_arrival(self):
        real_popen = subprocess.Popen
        with tempfile.TemporaryDirectory(prefix='gate-storage-native-arrival-') as folder:
            launch = pathlib.Path(folder) / 'arrival.py'
            launch.write_text('import os,sys,time\ntime.sleep(2.2)\n'
                              'os.execv(sys.argv[1],sys.argv[1:])\n')
            def delayed(argv, **kwargs):
                return real_popen([sys.executable, str(launch), *argv], **kwargs)
            with patch.object(storage.subprocess, 'Popen', delayed):
                self.test_oversized_and_timed_out_real_children_do_not_pass()

    def test_timeout_control_survives_parent_observer_descheduling(self):
        real_popen = subprocess.Popen
        def parent_descheduled(argv, **kwargs):
            child = real_popen(argv, **kwargs)
            if argv[-1] == 'import time; time.sleep(5)':
                # The actual child is running before only its observer waits.
                time.sleep(1.2)
            return child
        with patch.object(storage.subprocess, 'Popen', parent_descheduled):
            self.test_oversized_and_timed_out_real_children_do_not_pass()

    def test_timeout_control_detects_incomplete_without_child_cancellation(self):
        real_command = storage.native_command
        def not_cancelled(argv, timeout):
            if argv[-1] == 'import time; time.sleep(5)':
                return 0, b'', b'', False
            return real_command(argv, timeout)
        with patch.object(storage, 'native_command', not_cancelled):
            with self.assertRaises(AssertionError):
                self.test_oversized_and_timed_out_real_children_do_not_pass()

    def test_whole_census_binds_job_uid_and_container_lifetime(self):
        uid='12345678-1234-4234-8234-123456789012'
        job='22345678-1234-4234-8234-123456789012'
        packet='32345678-1234-4234-8234-123456789012'
        pods='continue\t\ngate-abc\t'+uid+'\tRunning\tw-1\tJob\tgate-abc\t'+job+'\tcontainerd://'+'a'*64+'\t0\n'
        jobs='continue\t\ngate-abc\t'+job+'\t'+packet+'\n'
        result=storage.cohort(pods,jobs,'w-1')
        self.assertEqual(result[0]['packet'],packet)
        for bad in (pods.replace(job,uid),pods.replace('continue\t','continue\tmore'),pods.replace('containerd://','invalid://'),pods+pods.split('\n',1)[1]):
            with self.assertRaises(ValueError): storage.cohort(bad,jobs,'w-1')

    def test_raw_chunks_and_manifest_use_existing_attach_readback_door(self):
        record={'schema':1,'state':'unavailable','readings':[], 'payload':'z'*(1024*1024+100)}
        calls=[]
        raw_parts=[]
        def execute(argv,timeout):
            self.assertEqual(argv[:3],['boss','attach','12345678-1234-4234-8234-123456789012'])
            raw=pathlib.Path(argv[3]).read_bytes(); calls.append(argv)
            self.assertLess(len(raw),2096128)
            if 'part-' in argv[3]: raw_parts.append(raw)
            return 0,b'actual attach readback command receipt\n',b'',True
        with tempfile.TemporaryDirectory(prefix='gate-storage-attach-') as folder:
            work=pathlib.Path(folder)
            storage.attach_record(work,'12345678-1234-4234-8234-123456789012',record,execute,lambda:10)
            self.assertEqual(b''.join(raw_parts),(work/'observation.json').read_bytes())
            manifest=json.loads((work/'observation-manifest.json').read_bytes())
            self.assertEqual(len(manifest['parts']),2)
            self.assertEqual(len(calls),3)
            self.assertNotIn('base64',manifest['parts'][0])
            with self.assertRaises(ValueError):
                storage.attach_record(work,'12345678-1234-4234-8234-123456789012',record,lambda a,t:(1,b'',b'refused',True),lambda:10)
            self.assertTrue((work/'observation.json').exists())

    def test_record_derivation_is_pure_repeatable_and_lifetime_bound(self):
        r=example_record()
        before=copy.deepcopy(r)
        result=storage.derive(r)
        self.assertEqual(result['state'],'observed')
        self.assertEqual(result,storage.derive(r))
        self.assertEqual(r,before)
        self.assertEqual(result['memory'][0]['dirty_bytes'],1024)
        self.assertEqual(result['pressure']['io']['some_stall_us'],15000)
        for version in ('Client:\n Tag: v1.13.8\nServer:\n Tag: v1.15.0\n',
                        'unparsed but nonempty native output\n'):
            bad=copy.deepcopy(r)
            next(row for row in bad['readings'] if row['operation']=='versions')['stdout']=storage.byte_record(version.encode())
            self.assertEqual(storage.derive(bad)['state'],'unavailable')
        for path,value in [('sample1:/proc/sys/kernel/random/boot_id','22345678-1234-4234-8234-123456789012\n'),('sample1:/proc/uptime','0 0\n'),('sample1:/proc/diskstats','259 7 nvme1n1 20 0 200 100 30 0 300 140 0 40 200 0 0 0 0\n')]:
            bad=copy.deepcopy(r)
            next(row for row in bad['readings'] if row['operation']==path)['stdout']=storage.byte_record(value.encode())
            self.assertEqual(storage.derive(bad)['state'],'unavailable')
        bad=copy.deepcopy(r); bad['readings'].append(copy.deepcopy(bad['readings'][0]))
        with self.assertRaises(ValueError): storage.derive(bad)
        bad=copy.deepcopy(r); bad['readings'][0]['stdout']['sha256']='0'*64
        with self.assertRaises(ValueError): storage.derive(bad)

    def test_complete_collector_uses_exact_native_argv_and_replays(self):
        port=NativeFixture()
        recorder=storage.Recorder(port.execute,clock=lambda:port.now,wall=lambda:'synthetic-wall')
        collector=storage.Collector(recorder,['kubectl'],TALOS,'registry-helper',sleep=port.sleep)
        record=collector.collect()
        self.assertEqual(record['state'],'observed',record.get('reason'))
        result=storage.derive(record)
        self.assertEqual(result['state'],'observed',result)
        self.assertTrue(result['cohort_stable'])
        self.assertEqual(result['media_health'],'unavailable')
        self.assertEqual(storage.derive(json.loads(json.dumps(record))),result)
        self.assertTrue(all('exec' not in call and 'machineconfig' not in call for call in port.calls))
        self.assertLessEqual(len(port.calls),128)
        self.assertEqual(port.calls[0],['sha256sum','--','/synthetic/talosctl'])
        self.assertEqual(port.calls[1],['kubectl','get','pvc','gate','-n','boss-dev','-o','json','--request-timeout=20s'])
        self.assertTrue(any(storage.PODS in call for call in port.calls))
        self.assertTrue(any(storage.JOBS in call for call in port.calls))
        self.assertTrue(any('volumestatus' in call and 'u-gate' in call for call in port.calls))
        self.assertIn('unavailable',record['availability']['cgroup_generation'])

    def test_changed_identity_unread_cohort_and_bound_refuse_full_observation(self):
        for mode in ('identity-drift','cohort-partial','native-refusal','budget'):
            port=NativeFixture(mode)
            recorder=storage.Recorder(port.execute,clock=lambda:port.now,wall=lambda:'synthetic-wall')
            collector=storage.Collector(recorder,['kubectl'],TALOS,'registry-helper',sleep=port.sleep)
            record=collector.collect()
            self.assertEqual(record['state'],'unavailable',mode)
            self.assertEqual(storage.derive(record)['state'],'unavailable',mode)


class NativeFixture:
    """Exact native projection/stream shapes, no cluster or candidate process env."""
    def __init__(self,mode='normal'):
        self.mode,self.now,self.calls,self.disk_reads,self.volume_reads=mode,0.0,[],0,0
        self.uid='12345678-1234-4234-8234-123456789012'
        self.job='22345678-1234-4234-8234-123456789012'
        self.packet='32345678-1234-4234-8234-123456789012'

    def sleep(self,seconds): self.now+=seconds

    def execute(self,argv,timeout):
        self.calls.append(argv)
        self.now+=160 if self.mode=='budget' else 0.05
        def ok(body): return 0,(json.dumps(body).encode() if isinstance(body,dict) else body),b'',True
        if self.mode=='native-refusal': return 23,b'',b'original refusal bytes\n',True
        if argv == ['sha256sum', '--', '/synthetic/talosctl']:
            return ok(('a' * 64 + '  /synthetic/talosctl\n').encode())
        if argv[0]=='kubectl':
            self.assert_kube(argv)
            if argv[2]=='pvc':
                return ok({'metadata':{'uid':self.uid,'name':'gate','namespace':'boss-dev'},'spec':{'volumeName':'gate-w-1'},'status':{'phase':'Bound'}})
            if argv[2]=='pv':
                return ok({'metadata':{'uid':self.job,'name':'gate-w-1'},'spec':{'claimRef':{'uid':self.uid,'name':'gate','namespace':'boss-dev'},'local':{'path':'/var/mnt/gate'},'nodeAffinity':{'required':{'nodeSelectorTerms':[{'matchExpressions':[{'key':'kubernetes.io/hostname','operator':'In','values':['w-1']}]}]}}}})
            if argv[2]=='pods':
                assert argv[-2]==storage.PODS
                header='continue\tmore\n' if self.mode=='cohort-partial' else 'continue\t\n'
                return ok((header+'gate-abc\t'+self.uid+'\tRunning\tw-1\tJob\tgate-abc\t'+self.job+'\tcontainerd://'+'a'*64+'\t0\n').encode())
            if argv[2]=='jobs':
                assert argv[-2]==storage.JOBS
                return ok(('continue\t\ngate-abc\t'+self.job+'\t'+self.packet+'\n').encode())
        if argv[0]=='bash':
            assert argv==['bash','registry-helper','w-1']
            return ok({'address':'192.0.2.1','role':'talos-worker','node':{'metadata':{'name':'w-1','uid':self.uid,'labels':{}},'status':{'addresses':[{'type':'InternalIP','address':'192.0.2.1'}]}}})
        assert argv[:len(TALOS)+2]==TALOS+['-n','192.0.2.1'],argv
        args=argv[len(TALOS)+2:]
        if args==['get','volumestatus','u-gate','-o','json']:
            self.volume_reads+=1
            return ok({'node':'192.0.2.1','metadata':{'id':'u-gate','type':'VolumeStatuses.block.talos.dev'},'spec':{'phase':'ready','filesystem':'xfs','location':'/dev/nvme1n1p1','parentLocation':'/dev/nvme1n1','partitionUUID':'changed' if self.mode=='identity-drift' and self.volume_reads>1 else self.uid,'mountSpec':{'targetPath':'gate','parentId':'/var/mnt'}}})
        if args==['get','disks','-o','json']:
            return ok({'node':'192.0.2.1','metadata':{'id':'nvme1n1','type':'Disks.block.talos.dev'},'spec':{'dev_path':'/dev/nvme1n1','wwid':'eui.synthetic','serial':'synthetic','model':'synthetic','bus_path':'/pci0000:00/0000:22:00.0/nvme'}})
        if args==['version']: return ok(b'Client:\n Tag: v1.13.8\nServer:\n Tag: v1.13.8\n')
        if args==['list','/sys/class/nvme/nvme1/device/hwmon','--depth=1','--type=d']: return ok(b'.\nhwmon3\nNODE   NAME\n')
        assert len(args)==2 and args[0]=='read',args
        path=args[1]
        if path=='/proc/sys/kernel/random/boot_id': return ok((self.uid+'\n').encode())
        if path=='/proc/uptime': return ok((str(100+self.now)+' 0\n').encode())
        if path=='/proc/version': return ok(b'Linux version synthetic-kernel\n')
        if path=='/proc/diskstats':
            self.disk_reads+=1; i=self.disk_reads
            return ok(f'259 5 nvme1n1 {i*10} 0 {i*100} {i*50} {i*20} 0 {i*200} {i*70} 0 {i*20} {i*100} 0 0 0 0\n259 6 nvme1n1p1 {i*10} 0 {i*100} {i*50} {i*20} 0 {i*200} {i*70} 0 {i*20} {i*100} 0 0 0 0\n'.encode())
        if path=='/proc/meminfo': return ok(b'Dirty: 1 kB\nWriteback: 2 kB\n')
        if path.startswith('/proc/pressure/') or path.endswith('/io.pressure'): return ok(f'some avg10=0.0 avg60=0.0 avg300=0.0 total={int(self.now*1000)}\nfull avg10=0.0 avg60=0.0 avg300=0.0 total={int(self.now*10)}\n'.encode())
        if path.endswith('/io.stat'): return ok(b'259:5 rbytes=1 wbytes=2 rios=1 wios=1\n')
        if path.endswith('/io.max'): return ok(b'')
        if path.endswith('/memory.stat'): return ok(b'file_dirty 0\nfile_writeback 0\n')
        if path.endswith('/cpu.stat'): return ok(b'usage_usec 1\n')
        if path.endswith('/state'): return ok(b'live\n')
        if path.endswith('/name'): return ok(b'nvme\n')
        if '/hwmon/' in path: return ok(b'32000\n')
        if '/aer_dev_' in path: return ok(b'TOTAL_ERR_COR 0\n')
        raise AssertionError('unexpected native path '+path)

    def assert_kube(self,argv):
        assert argv[1]=='get' and argv[-1]=='--request-timeout=20s',argv


def example_record():
    port = NativeFixture()
    return storage.Collector(storage.Recorder(port.execute, clock=lambda: port.now),
                             ['kubectl'], TALOS, 'registry-helper', sleep=port.sleep).collect()


if __name__=='__main__':
    unittest.main()
