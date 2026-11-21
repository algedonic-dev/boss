"""Native-shaped controls for the fixed, passkey-bound filesystem trim (backlog b1520f80 part b).

The fixture is the snapshot purge's own (instance_snapshot_purge_test.py):
the trim admits a volume through the same identity and health checks, so
it is held to the same native shapes, plus the facts only a trim reads.
"""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import unittest

HERE = Path(__file__).parent


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, HERE / file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


trim = load('trim', 'instance-volume-trim.py')
shapes = load('purge_shapes', 'instance_snapshot_purge_test.py')
VOL = shapes.VOL
APPROVAL = 'infra/ops/verbs/trim-instance-volume.json'


def fixture():
    b = shapes.fixture()
    b['pvc']['spec']['volumeMode'] = 'Filesystem'
    b['volume']['spec'].update(accessMode='rwo', frontend='blockdev', disableFrontend=False, encrypted=False,
                               unmapMarkSnapChainRemoved='ignored', size='21474836480')
    b['volume']['status'].update(frontendDisabled=False, actualSize=9000)
    b['setting'] = shapes.obj('Setting', trim.SETTING, value='false')
    volumes = [{'name': 'data', 'persistentVolumeClaim': {'claimName': 'db'}}]
    b['pods'] = [shapes.obj('Pod', 'postgres-0', 'boss', spec={'volumes': volumes, 'nodeName': 'cp-1'},
                            status={'phase': 'Running'})]
    for key in ('snapshots', 'snapshot_crs', 'nodes'):
        del b[key]
    return b


def sized(b, actual):
    row = copy.deepcopy(b)
    row['volume']['status']['actualSize'] = actual
    return row


class Plan(unittest.TestCase):
    def test_plan_is_trim_only_and_freezes_identity_not_live_bytes(self):
        b = fixture(); original = copy.deepcopy(b)
        plan = trim.render(b, 'boss', 'db')
        self.assertEqual(b, original)
        self.assertEqual(plan['operation'], 'trimFilesystem')
        self.assertEqual(plan['volume']['name'], VOL)
        self.assertEqual(plan['volume']['size'], '21474836480')
        self.assertEqual(plan['mounted_by'], [{'name': 'postgres-0', 'uid': b['pods'][0]['metadata']['uid'], 'node': 'cp-1'}])
        self.assertEqual(plan['unmap_mark_snap_chain_removed'],
                         {'volume': 'ignored', 'setting': 'false', 'resolved': 'disabled'})
        self.assertNotIn('9000', json.dumps(plan))
        self.assertEqual(plan, trim.render(sized(b, 1), 'boss', 'db'), 'actualSize moves every second and is never signed')
        b['volume']['metadata']['uid'] = '22222222-2222-4222-8222-222222222222'
        self.assertNotEqual(plan, trim.render(b, 'boss', 'db'))

    def test_approval_hash_is_exact_stdout_plan_bytes(self):
        plan = trim.render(fixture(), 'boss', 'db')
        stdout = (json.dumps(plan, sort_keys=True, indent=2) + '\n').encode()
        self.assertEqual(trim.digest(plan), hashlib.sha256(stdout).hexdigest())

    def test_a_trim_that_would_mark_snapshots_removed_is_refused(self):
        # Longhorn's unmapMarkSnapChainRemoved marks the snapshot chain
        # removed on a trim: a snapshot decision, outside this verb.
        b = fixture(); b['volume']['spec']['unmapMarkSnapChainRemoved'] = 'enabled'
        with self.assertRaisesRegex(ValueError, 'snapshot chain'): trim.render(b, 'boss', 'db')
        b = fixture(); b['setting']['value'] = 'true'
        with self.assertRaisesRegex(ValueError, 'snapshot chain'): trim.render(b, 'boss', 'db')
        b = fixture(); b['volume']['spec']['unmapMarkSnapChainRemoved'] = 'disabled'; b['setting']['value'] = 'true'
        self.assertEqual(trim.render(b, 'boss', 'db')['unmap_mark_snap_chain_removed']['resolved'], 'disabled')
        for value in ('', 'maybe', None):
            b = fixture(); b['setting']['value'] = value
            with self.subTest(setting=value), self.assertRaises(ValueError): trim.render(b, 'boss', 'db')
        b = fixture(); b['volume']['spec']['unmapMarkSnapChainRemoved'] = 'sometimes'
        with self.assertRaises(ValueError): trim.render(b, 'boss', 'db')
        b = fixture(); b['setting']['metadata']['name'] = 'another-setting'
        with self.assertRaises(ValueError): trim.render(b, 'boss', 'db')

    def test_only_a_mounted_filesystem_on_the_attached_node_is_trimmed(self):
        for change in [lambda b: b['pods'].clear(),
                       lambda b: b['pods'][0]['status'].update(phase='Pending'),
                       lambda b: b['pods'][0]['spec'].update(nodeName='w-1'),
                       lambda b: b['pods'][0]['spec']['volumes'][0]['persistentVolumeClaim'].update(claimName='other')]:
            b = fixture(); change(b)
            with self.assertRaisesRegex(ValueError, 'mounted'): trim.render(b, 'boss', 'db')

    def test_shapes_longhorn_cannot_trim_or_this_verb_never_tested_are_refused(self):
        for change in [lambda b: b['pvc']['spec'].update(volumeMode='Block'),
                       lambda b: b['pvc']['spec'].pop('volumeMode'),
                       lambda b: b['volume']['spec'].update(accessMode='rwx'),
                       lambda b: b['volume']['spec'].update(frontend=''),
                       lambda b: b['volume']['spec'].update(disableFrontend=True),
                       lambda b: b['volume']['status'].update(frontendDisabled=True),
                       lambda b: b['volume']['status'].pop('frontendDisabled'),
                       lambda b: b['volume']['spec'].update(encrypted=True),
                       lambda b: b['volume']['spec'].update(size='twenty')]:
            b = fixture(); change(b)
            with self.assertRaises(ValueError): trim.render(b, 'boss', 'db')

    def test_the_purge_admission_is_inherited_whole(self):
        for change in [lambda b: b['engines'][0]['status'].update(rebuildStatus={'x': {'isRebuilding': True}}),
                       lambda b: b['engines'][0]['status'].update(backupStatus={'x': {'state': 'in_progress', 'progress': 5}}),
                       lambda b: b['engines'][0]['status'].update(purgeStatus={'x': {'isPurging': True}}),
                       lambda b: b['volume']['status'].update(robustness='degraded'),
                       lambda b: b['volume']['spec'].update(image='longhornio/longhorn-engine:v1.11.2'),
                       lambda b: b['replicas'][0]['spec'].update(failedAt='2026-11-01T00:00:00Z'),
                       lambda b: b['pv']['spec']['claimRef'].update(uid='other')]:
            b = fixture(); change(b)
            with self.assertRaises(ValueError): trim.render(b, 'boss', 'db')


class Execute(unittest.TestCase):
    def test_a_moved_or_wrong_plan_trims_nothing(self):
        b = fixture(); approved = trim.digest(trim.render(b, 'boss', 'db')); actions = []
        with self.assertRaisesRegex(ValueError, 'approved plan differs'):
            trim.execute(lambda: b, actions.append, 'boss', 'db', 'f' * 64, sleep=lambda _: None)
        moved = copy.deepcopy(b); moved['pods'][0]['metadata']['uid'] = '22222222-2222-4222-8222-222222222222'
        reads = iter([b, moved])
        with self.assertRaisesRegex(ValueError, 'nothing was trimmed'):
            trim.execute(lambda: next(reads), actions.append, 'boss', 'db', approved, sleep=lambda _: None)
        self.assertEqual(actions, [])

    def test_actual_size_is_read_back_before_and_after(self):
        b = fixture(); approved = trim.digest(trim.render(b, 'boss', 'db')); actions = []; slept = []
        reads = iter([sized(b, 9000), sized(b, 8000)] + [sized(b, n) for n in (8000, 5000, 4000, 4000, 4000, 4000)])
        result = trim.execute(lambda: next(reads), actions.append, 'boss', 'db', approved, sleep=slept.append)
        self.assertEqual(actions, [VOL])
        self.assertEqual(result['state'], 'trim-acknowledged')
        self.assertEqual(result['actual_size_before'], 8000, 'before is the read that immediately preceded the trim')
        self.assertEqual(result['actual_size_after'], 4000)
        self.assertEqual(result['readings_after'], [8000, 5000, 4000, 4000, 4000, 4000])
        self.assertEqual(sum(slept), trim.SETTLE_SECONDS)
        self.assertFalse(result['capacity_recovery_proven'])
        json.dumps(result)  # the record the wrapper reads is plain JSON

    def test_an_unmoved_size_is_reported_as_read_not_as_a_failure(self):
        b = fixture(); approved = trim.digest(trim.render(b, 'boss', 'db'))
        result = trim.execute(lambda: b, lambda _: None, 'boss', 'db', approved, sleep=lambda _: None)
        self.assertEqual((result['actual_size_before'], result['actual_size_after']), (9000, 9000))

    def test_an_unread_size_or_a_moved_identity_after_the_trim_is_unproven(self):
        b = fixture(); approved = trim.digest(trim.render(b, 'boss', 'db'))
        for after in [lambda r: r['volume']['status'].pop('actualSize'),
                      lambda r: r['volume']['status'].update(actualSize=True),
                      lambda r: r['volume']['status'].update(actualSize=-1),
                      lambda r: r['replicas'][0]['metadata'].update(uid='22222222-2222-4222-8222-222222222222'),
                      lambda r: r['engines'][0]['status'].update(rebuildStatus={'x': {'isRebuilding': True}})]:
            changed = copy.deepcopy(b); after(changed)
            reads = iter([b, b, changed]); actions = []
            with self.subTest(), self.assertRaisesRegex(trim.base.Unproven, 'trim requested; effect NOT proven'):
                trim.execute(lambda: next(reads), actions.append, 'boss', 'db', approved, sleep=lambda _: None)
            self.assertEqual(actions, [VOL])

    def test_a_refused_or_lost_trim_request_is_unproven_not_a_prewrite_refusal(self):
        b = fixture(); approved = trim.digest(trim.render(b, 'boss', 'db'))
        def lost(_):
            raise ValueError('native command failed or incomplete: volume is not attached')
        with self.assertRaisesRegex(trim.base.Unproven, 'not attached'):
            trim.execute(lambda: b, lost, 'boss', 'db', approved, sleep=lambda _: None)



def outcome_of_a_real_run():
    b = fixture(); approved = trim.digest(trim.render(b, 'boss', 'db'))
    reads = iter([b, sized(b, 107000000000)] + [sized(b, 99000000000)] * 6)
    return trim.execute(lambda: next(reads), lambda _: None, 'boss', 'db', approved, sleep=lambda _: None)


class Wrapper(unittest.TestCase):
    """The real wrapper, a fake adapter: the effect line is the wrapper's alone."""

    def run_wrapper(self, adapter_stdout, code, *args, registry='printf "brewery\\tboss\\nplayground\\tboss-playground\\n"'):
        import os
        import shutil
        import subprocess
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); forge = root / 'infra/forge'; forge.mkdir(parents=True)
            (root / 'infra/estate').mkdir(); (root / 'infra/cluster').mkdir(); (root / 'infra/lib').mkdir()
            shutil.copy(HERE / 'instance-volume-trim.sh', forge)
            shutil.copy(HERE.parent / 'lib/jq.sh', root / 'infra/lib')
            (root / 'infra/estate/ops-credentials.sh').write_text('ops_image_ready() { echo ready; }; ops_kubectl() { echo fake-kube; }\n')
            renderer = root / 'infra/cluster/render-instance.sh'
            renderer.write_text('#!/bin/sh\n' + registry + '\n'); renderer.chmod(0o700)
            (root / 'adapter-stdout').write_text(adapter_stdout)
            (forge / 'instance-volume-trim.py').write_text(
                'import os,sys\nfrom pathlib import Path\n'
                'print("seen:", os.environ["INSTANCE_TRIM_NAMESPACES"].replace("\\n", " "), os.environ["INSTANCE_TRIM_KUBECTL"], *sys.argv[1:], file=sys.stderr)\n'
                'sys.stdout.write((Path(__file__).parents[2] / "adapter-stdout").read_text())\n'
                'sys.exit(' + str(code) + ')\n')
            return subprocess.run(['bash', str(forge / 'instance-volume-trim.sh'), *args],
                                  capture_output=True, text=True, check=False)

    def effect(self):
        return json.loads((HERE.parent.parent / APPROVAL).read_text())['effect']

    def test_the_effect_is_printed_from_the_read_back_record_and_matches_the_verb(self):
        record = json.dumps(outcome_of_a_real_run(), sort_keys=True) + '\n'
        run = self.run_wrapper(record, 0, 'boss', 'db', 'a' * 64)
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertIn('seen: boss boss-playground fake-kube boss db ' + 'a' * 64, run.stderr)
        lines = run.stdout.splitlines()
        self.assertEqual(lines[0] + '\n', record)
        self.assertEqual(lines[1], 'instance-volume-trim: Longhorn acknowledged trimFilesystem on ' + VOL
                         + '; status.actualSize read back 107000000000 -> 99000000000 bytes over 60 s; per-disk free space is not read here')
        self.assertEqual([l for l in lines if re.search(self.effect(), l)], [lines[1]])

    def test_no_effect_without_a_complete_record_or_on_a_failed_adapter(self):
        good = outcome_of_a_real_run()
        # Silence first: jq-1.6's -e passes an input with no document.
        for stdout in ['', '\n', '   \n']:
            run = self.run_wrapper(stdout, 0, 'boss', 'db', 'a' * 64)
            with self.subTest(stdout=stdout):
                self.assertEqual(run.returncode, 1)
                self.assertIn('effect NOT proven', run.stderr)
        for broken in [{**good, 'state': 'refused-or-unproven'}, {**good, 'actual_size_after': None},
                       {k: v for k, v in good.items() if k != 'actual_size_before'}]:
            run = self.run_wrapper(json.dumps(broken) + '\n', 0, 'boss', 'db', 'a' * 64)
            with self.subTest(broken=broken.get('state')):
                self.assertEqual(run.returncode, 1)
                self.assertIn('effect NOT proven', run.stderr)
                self.assertFalse([l for l in run.stdout.splitlines() if re.search(self.effect(), l)])
        for code in (1, 78):
            run = self.run_wrapper(json.dumps(good) + '\n', code, 'boss', 'db', 'a' * 64)
            self.assertEqual(run.returncode, code)
            self.assertFalse([l for l in run.stdout.splitlines() if re.search(self.effect(), l)])

    def test_the_plan_bytes_pass_through_untouched(self):
        plan = json.dumps(trim.render(fixture(), 'boss', 'db'), sort_keys=True, indent=2) + '\n'
        run = self.run_wrapper(plan, 0, '--plan', 'boss', 'db')
        self.assertEqual(run.returncode, 0)
        self.assertEqual(run.stdout, plan)
        self.assertEqual(self.run_wrapper(plan, 78, '--plan', 'boss', 'db').returncode, 78)

    def test_an_unread_instance_registry_reads_nothing(self):
        run = self.run_wrapper('', 0, 'boss', 'db', 'a' * 64, registry='exit 3')
        self.assertEqual(run.returncode, 1)
        self.assertIn('CANNOT ANSWER', run.stderr)
        self.assertNotIn('seen:', run.stderr)
        self.assertEqual(run.stdout, '')


class Wire(unittest.TestCase):
    def native(self, directory):
        return trim.Native(['trusted-kubectl'], Path(directory))

    def test_the_one_fixed_action_and_its_acknowledgement(self):
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            native = self.native(d); calls = []
            def call(args, seconds=20):
                calls.append((args, seconds)); return {'type': 'volume', 'name': VOL}
            native.call = call
            native.trim(VOL)
            self.assertTrue(native.mutation_attempted)
            self.assertEqual(calls, [(['create', '--raw', trim.base.PROXY + VOL + '?action=trimFilesystem', '-f', '/dev/null',
                                       '--request-timeout=' + str(trim.TRIM_SECONDS) + 's'], trim.TRIM_SECONDS)])
            for volume in ('https://elsewhere', 'pvc-x', VOL + '?action=snapshotPurge'):
                with self.assertRaises(ValueError): native.trim(volume)
            native.call = lambda *_, **__: {'type': 'error', 'message': 'not performed'}
            with self.assertRaisesRegex(ValueError, 'acknowledgement'): native.trim(VOL)

    def test_the_read_is_the_fixed_inventory_and_never_an_action(self):
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            native = self.native(d); gets = []; lists = []
            b = fixture()
            rows = {('pvc', 'db'): b['pvc'], ('pv', VOL): b['pv'], ('volumes.longhorn.io', VOL): b['volume'],
                    ('settings.longhorn.io', trim.SETTING): b['setting']}
            def get(resource, name, namespace):
                gets.append((resource, name, namespace)); return copy.deepcopy(rows[(resource, name)])
            def listing(resource, kind, namespace):
                lists.append((resource, namespace)); return []
            native.get = get; native.listing = listing
            native.call = lambda *_, **__: self.fail('a read reached a raw call')
            result = native.read('boss', 'db')
            self.assertEqual(result['setting'], b['setting'])
            self.assertEqual([g[0] for g in gets], ['pvc', 'pv', 'volumes.longhorn.io', 'settings.longhorn.io'])
            self.assertEqual(sorted(lists), sorted([('replicas.longhorn.io', 'longhorn-system'), ('engines.longhorn.io', 'longhorn-system'),
                                                    ('backups.longhorn.io', 'longhorn-system'), ('jobs', 'boss'), ('pods', 'boss')]))
            self.assertFalse(native.mutation_attempted)

    def test_a_long_call_keeps_the_shared_byte_and_time_bounds(self):
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            native = self.native(d); seen = []
            original = trim.base.bounded.native_command
            try:
                trim.base.bounded.native_command = lambda argv, timeout: seen.append(timeout) or (0, b'{"type":"volume","name":"' + VOL.encode() + b'"}', b'', True)
                native.trim(VOL)
                native.get('pvc', 'db', 'boss')
            finally:
                trim.base.bounded.native_command = original
            self.assertGreater(seen[0], 20)
            self.assertLessEqual(seen[0], trim.TRIM_SECONDS)
            self.assertLessEqual(seen[1], 20, 'an ordinary read keeps its 20 s bound')

    def test_evidence_files_say_trim(self):
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            (work / 'native-1.json').write_text('{"sequence":1,"begin_monotonic":1,"end_monotonic":2}\n')
            names = []
            def attach(argv, timeout):
                names.append(Path(argv[-1]).name); return 0, b'confirmed: byte-identical\n', b'', True
            trim.base.publish(work, '11111111-1111-4111-8111-111111111111', {'state': 'fixture'}, execute=attach, label='trim')
            self.assertEqual(names, ['trim-evidence-0.json.bytes', 'trim-evidence-manifest.json'])


if __name__ == '__main__':
    unittest.main()
