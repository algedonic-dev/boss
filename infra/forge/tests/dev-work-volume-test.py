"""Fixed-purpose dev work expansion: native-shaped inputs and real shared ledger."""
import contextlib
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import unittest
import tempfile
import base64
import gzip
import hashlib
import time
import types
import unittest.mock
import subprocess

ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location('dev_work_volume', ROOT / 'infra/forge/dev-work-volume.py')
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
GIB = 1073741824


def obj(kind, name, namespace=None, **values):
    metadata = {'name': name, 'uid': 'uid-' + name}
    if namespace:
        metadata['namespace'] = namespace
    version = ('longhorn.io/v1beta2' if kind in ('Volume','Replica','Node','Setting') else
               'storage.k8s.io/v1' if kind=='StorageClass' else 'v1')
    return dict(kind=kind, apiVersion=version, metadata=metadata, **values)


def inputs():
    volume = 'pvc-12345678-1234-1234-1234-123456789abc'
    ready = [{'type': 'Ready', 'status': 'True'}, {'type': 'Schedulable', 'status': 'True'}]
    result = {
        'pvc': obj('PersistentVolumeClaim', 'boss-dev-work', 'boss-dev',
                   spec={'storageClassName': 'longhorn-dev-disposable', 'volumeName': volume,
                         'volumeMode': 'Filesystem', 'accessModes': ['ReadWriteOnce'],
                         'resources': {'requests': {'storage': '64Gi'}}},
                   status={'phase': 'Bound', 'capacity': {'storage': '64Gi'}, 'conditions': []}),
        'pv': obj('PersistentVolume', volume, spec={'claimRef': {'namespace': 'boss-dev', 'name': 'boss-dev-work', 'uid': 'uid-boss-dev-work'},
                  'csi': {'driver': 'driver.longhorn.io', 'volumeHandle': volume, 'fsType': 'ext4'},
                  'capacity': {'storage': '64Gi'}}),
        'sc': obj('StorageClass', 'longhorn-dev-disposable', provisioner='driver.longhorn.io', allowVolumeExpansion=True),
        'volume': obj('Volume', volume, 'longhorn-system', spec={'size': str(64 * GIB), 'numberOfReplicas': 1,
                     'frontend': 'blockdev', 'dataEngine': 'v1', 'encrypted': False, 'accessMode': 'rwo'},
                     status={'state': 'attached', 'robustness': 'healthy', 'currentNodeID': 'w-1',
                             'kubernetesStatus': {'namespace': 'boss-dev', 'pvcName': 'boss-dev-work', 'pvName': volume}}),
        'replicas': {'kind': 'List', 'metadata': {}, 'items': [obj('Replica', 'replica-one', 'longhorn-system',
                     spec={'volumeName': volume, 'nodeID': 'w-1', 'diskID': 'disk-one', 'active': True,
                           'healthyAt': '2026-10-15T12:00:00Z', 'failedAt': ''})]},
        'nodes': {'kind': 'List', 'metadata': {}, 'items': [obj('Node', 'w-1', 'longhorn-system',
                  spec={'allowScheduling': True, 'disks': {'disk': {'storageReserved': 0, 'allowScheduling': True}}},
                  status={'conditions': ready, 'diskStatus': {'disk': {'diskUUID': 'disk-one',
                          'storageMaximum': 500 * GIB, 'storageScheduled': 80 * GIB,
                          'storageAvailable': 400 * GIB, 'conditions': ready}}})]},
        'overprovisioning': obj('Setting', 'storage-over-provisioning-percentage', 'longhorn-system', value='100'),
        'minimal_available': obj('Setting', 'storage-minimal-available-percentage', 'longhorn-system', value='25'),
        'pods': {'kind': 'List', 'metadata': {}, 'items': [obj('Pod', 'boss-dev-running', 'boss-dev',
                 spec={'nodeName': 'w-1', 'volumes': [{'name': 'work', 'persistentVolumeClaim': {'claimName': 'boss-dev-work'}}],
                       'containers': [{'name': 'dev', 'volumeMounts': [{'name': 'work', 'mountPath': '/work'}]}]},
                 status={'phase': 'Running', 'containerStatuses': [{'name': 'dev', 'containerID': 'containerd://stable', 'restartCount': 0,
                         'ready': True, 'state': {'running': {'startedAt': '2026-10-15T10:00:00Z'}}}]})]},
    }
    result['volume']['status']['currentImage'] = 'longhornio/longhorn-engine:v1.11.3'
    result['csi_pods'] = {'kind': 'List', 'metadata': {}, 'items': [obj('Pod', 'longhorn-csi-one', 'longhorn-system',
        spec={'nodeName': 'w-1', 'containers': [{'name': 'longhorn-csi-plugin', 'image': 'longhornio/longhorn-manager:v1.11.3'}]},
        status={'phase': 'Running', 'containerStatuses': [{'name': 'longhorn-csi-plugin', 'ready': True,
            'imageID': 'docker-pullable://longhornio/longhorn-manager@sha256:'+'a'*64,
            'containerID': 'containerd://csi', 'state': {'running': {'startedAt': '2026-10-15T10:00:00Z'}}}]})]}
    result['mount'] = {'filesystem': 'ext4', 'source': '/dev/longhorn/'+volume, 'mountpoint': '/work',
        'device': '8:16', 'available_bytes': 5*GIB, 'capacity_bytes': 64*GIB, 'boot_id': 'boot-id',
        'processes': [{'pid': 123, 'starttime': 1234, 'name': 'codex'}]}
    result['rights'] = {'patch_claim': True, 'read_mount': True}
    return result


STUB = '''import json,sys
from pathlib import Path
b=Path(__file__).parent
d=json.loads((b/'data.json').read_text())
a=sys.argv[1:]
with (b/'calls').open('a') as out:out.write(json.dumps(a)+'\\n')
if a[:2]==['auth','can-i']: print('yes');sys.exit(0)
if a[0]=='exec':
    print(json.dumps(d['mount']))
    if (b/'replace-pod').exists():
        d['pods']['items'][0]['metadata']['uid']='replacement'
        (b/'data.json').write_text(json.dumps(d))
    sys.exit(0)
if a[0]=='patch':
    d['pvc']['spec']['resources']['requests']['storage']='96Gi'
    if not (b/'pending').exists():
        d['pvc']['status']['capacity']['storage']='96Gi'
        d['pv']['spec']['capacity']['storage']='96Gi'
        d['volume']['spec']['size']=str(96*1073741824)
        d['mount'].update(capacity_bytes=96*1073741824,available_bytes=29*1073741824)
    (b/'data.json').write_text(json.dumps(d))
    print(json.dumps(d['pvc']));sys.exit(0)
mapping={'pvc':'pvc','pv':'pv','storageclass':'sc','volumes.longhorn.io':'volume',
         'replicas.longhorn.io':'replicas','nodes.longhorn.io':'nodes'}
if a[:2]==['get','pod']:print(json.dumps(d['pods']['items'][0]));sys.exit(0)
listed=a[0]=='get' and len(a)>2 and a[2]=='-n'
if listed and 'fail-list:'+a[1] in d:sys.stderr.write(d['fail-list:'+a[1]]);sys.exit(1)
if a[:2]==['get','pods']:key='pods' if a[a.index('-n')+1]=='boss-dev' else 'csi_pods'
elif a[:2]==['get','settings.longhorn.io']:key='overprovisioning' if a[2]=='storage-over-provisioning-percentage' else 'minimal_available'
elif listed and 'list:'+a[1] in d:key='list:'+a[1]
elif a[0]=='get' and a[1] in mapping:key=mapping[a[1]]
else:raise RuntimeError('unexpected native command '+repr(a))
print(json.dumps(d[key]))
'''


def stubbed(directory, data, noise=0):
    """A kubectl stand-in over native-shaped data; noise pods make each read real-sized."""
    base=Path(directory)
    for index in range(noise):
        pod=obj('Pod','noise-%d' % index,'boss-dev',spec={'containers':[{'name':'other'}]},status={'phase':'Running'})
        pod['metadata']['annotations']={'noise':os.urandom(256).hex()}
        data['pods']['items'].append(pod)
    (base/'data.json').write_text(json.dumps(data))
    (base/'kubectl.py').write_text(STUB)
    return MODULE.Native([os.sys.executable,str(base/'kubectl.py')],budget_seconds=120)


def calls(directory):
    return [json.loads(line) for line in (Path(directory)/'calls').read_text().splitlines()]


class Planning(unittest.TestCase):
    def test_worker_diagnostic_uses_bounded_existing_reads_without_changing_signed_plan(self):
        data = inputs()
        before = self.plan(data, '96Gi')
        native = MODULE.Native(['unused'])
        native.room = lambda: 2
        empty = {'kind': 'List', 'metadata': {}, 'items': []}
        requests = []
        def get(resource, name=None, namespace=None):
            requests.append((resource, name, namespace))
            return empty
        native.get = get
        result = native.worker_scheduling(data)
        self.assertEqual(result['state'], 'OBSERVED')
        self.assertEqual(result['workers'], [])
        self.assertEqual(requests, [('pvc', None, 'boss-dev'),
                                    ('volumes.longhorn.io', None, 'longhorn-system')])
        self.assertEqual(self.plan(data, '96Gi'), before)
        def refused(*args, **kwargs):
            raise ValueError('native existing credential refuses list')
        native.get = refused
        self.assertEqual(native.worker_scheduling(data)['state'], 'UNKNOWN')
        self.assertIn('credential refuses', native.worker_scheduling(data)['cause'])
        self.assertEqual(self.plan(data, '96Gi'), before)
        native.room = lambda: 0
        self.assertIn('no room', native.worker_scheduling(data)['cause'])

    def test_dev_build_diagnostic_joins_exact_claim_and_volume_or_reports_unknown(self):
        data = inputs()
        name = 'dev-build-0123456789abcdef'
        worker = obj('Pod', name+'-pod', 'boss-dev',
                     spec={'volumes': [{'persistentVolumeClaim': {'claimName': name}}]},
                     status={'phase': 'Pending'})
        worker['metadata']['labels'] = {'boss.dev/purpose': 'isolated-build', 'boss.dev/session': name}
        claim = copy.deepcopy(data['pvc'])
        claim['metadata'].update(name=name, uid='worker-claim-uid')
        volume = copy.deepcopy(data['volume'])
        volume['status']['kubernetesStatus']['pvcName'] = name
        rows = MODULE.dev_build_scheduling(
            {'kind': 'List', 'metadata': {}, 'items': [worker]},
            {'kind': 'List', 'metadata': {}, 'items': [claim]},
            {'kind': 'List', 'metadata': {}, 'items': [volume]}, data)
        self.assertEqual(rows['workers'][0]['claim_uid'], 'worker-claim-uid')
        self.assertEqual(rows['workers'][0]['state'], 'OBSERVED')
        self.assertEqual(rows['workers'][0]['volume'], data['pvc']['spec']['volumeName'])
        self.assertTrue(rows['workers'][0]['ledger'])
        self.assertEqual(rows['workers'][0]['ledger'][0]['required_bytes'], 0)
        for variant in ('missing', 'duplicate', 'binding', 'partial', 'selector'):
            pods = {'kind': 'List', 'metadata': {}, 'items': [copy.deepcopy(worker)]}
            claims = {'kind': 'List', 'metadata': {}, 'items': [copy.deepcopy(claim)]}
            volumes = {'kind': 'List', 'metadata': {}, 'items': [copy.deepcopy(volume)]}
            if variant == 'missing': claims['items'] = []
            if variant == 'duplicate': claims['items'] *= 2
            if variant == 'binding': volumes['items'][0]['status']['kubernetesStatus']['pvcName'] = 'foreign'
            if variant == 'partial': claims['metadata']['continue'] = 'next'
            if variant == 'selector': pods['items'][0]['metadata']['labels']['boss.dev/session'] = 'foreign'
            with self.subTest(variant=variant):
                observed = MODULE.dev_build_scheduling(pods, claims, volumes, data)
                self.assertEqual(observed['state'], 'UNKNOWN')
                self.assertIn('cause', observed)

    def test_relief_accepts_65_through_128_from_the_existing_64Gi_claim(self):
        data = inputs()
        data['pvc']['spec']['resources']['requests']['storage'] = '64Gi'
        data['pvc']['status']['capacity']['storage'] = '64Gi'
        data['pv']['spec']['capacity']['storage'] = '64Gi'
        data['volume']['spec']['size'] = str(64 * GIB)
        data['mount']['capacity_bytes'] = 64 * GIB
        data['mount']['available_bytes'] = 19 * GIB
        for requested in ('65Gi', '96Gi', '128Gi'):
            with self.subTest(requested=requested):
                proposal = self.plan(data, requested)
                self.assertEqual(proposal['target_bytes'], MODULE.size(requested))
                self.assertEqual(proposal['write']['namespace'], 'boss-dev')
                self.assertEqual(proposal['write']['claim'], 'boss-dev-work')

    def setUp(self):
        # Admission arithmetic uses the real shared checker. Its production
        # ten-second deadline has a separate, deterministic boundary test below;
        # scheduler delay while starting bash/jq is not an arithmetic result.
        real = MODULE.subprocess
        checker = ['bash', str(ROOT/'infra/lib/check-longhorn-expansion.sh')]
        def semantic_run(argv, **kwargs):
            if argv == checker:
                self.assertEqual(kwargs['timeout'], 10)
                kwargs = dict(kwargs, timeout=None)
            return real.run(argv, **kwargs)
        proxy = types.SimpleNamespace(**{name: getattr(real, name) for name in dir(real)})
        proxy.run = semantic_run
        patch = unittest.mock.patch.object(MODULE, 'subprocess', proxy)
        patch.start()
        self.addCleanup(patch.stop)

    def test_checker_deadline_is_ten_seconds_and_refuses_before_any_patch(self):
        seen = []
        writes = []
        def timed_out(argv, **kwargs):
            self.assertEqual(argv, ['bash', str(ROOT/'infra/lib/check-longhorn-expansion.sh')])
            self.assertEqual(set(kwargs), {'input', 'text', 'capture_output', 'timeout', 'check'})
            self.assertEqual((kwargs['text'], kwargs['capture_output'], kwargs['timeout'], kwargs['check']),
                             (True, True, 10, False))
            ledger = json.loads(kwargs['input'])
            self.assertEqual(ledger['growth'], 32 * GIB)
            self.assertEqual(ledger['volume'], inputs()['volume'])
            seen.append(ledger)
            raise subprocess.TimeoutExpired(argv, 10, output='partial ledger', stderr='checker pending')
        proxy = types.SimpleNamespace(**{name: getattr(subprocess, name) for name in dir(subprocess)})
        proxy.run = timed_out
        with unittest.mock.patch.object(MODULE, 'subprocess', proxy), \
                self.assertRaises(subprocess.TimeoutExpired) as failure:
            MODULE.execute(lambda: inputs(), writes.append, '96Gi', 'a'*64)
        self.assertEqual(len(seen), 1)
        self.assertEqual(writes, [])
        self.assertEqual(failure.exception.output, 'partial ledger')
        self.assertEqual(failure.exception.stderr, 'checker pending')

    def test_docker_hub_qualified_longhorn_references_keep_original_signed_evidence(self):
        for engine_qualified in (False, True):
            for plugin_qualified in (False, True):
                data = inputs()
                engine = data['volume']['status']
                plugin = data['csi_pods']['items'][0]['spec']['containers'][0]
                if engine_qualified:
                    engine['currentImage'] = 'docker.io/' + engine['currentImage']
                if plugin_qualified:
                    plugin['image'] = 'docker.io/' + plugin['image']
                with self.subTest(engine=engine_qualified, plugin=plugin_qualified):
                    result = self.plan(data)
                    self.assertEqual(result['online_support']['engine'], engine['currentImage'])
                    self.assertEqual(result['online_support']['image_id'],
                                     data['csi_pods']['items'][0]['status']['containerStatuses'][0]['imageID'])

    def test_longhorn_reference_qualification_does_not_accept_other_images(self):
        for component in ('engine', 'manager'):
            familiar = 'longhornio/longhorn-' + component + ':v1.11.3'
            for value in ('evil.example/' + familiar, 'docker.io.evil.example/' + familiar,
                          'docker.io/library/' + familiar, 'index.docker.io/' + familiar,
                          'docker.io/' + familiar + '@sha256:' + 'a'*64,
                          familiar.replace('v1.11.3', 'v1.11.4'), familiar + ' ',
                          'DOCKER.IO/' + familiar, familiar.replace('longhornio/', 'other/'),
                          familiar.replace(':v1.11.3', ''), None, 113):
                data = inputs()
                if component == 'engine':
                    data['volume']['status']['currentImage'] = value
                else:
                    data['csi_pods']['items'][0]['spec']['containers'][0]['image'] = value
                with self.subTest(component=component, image=value), self.assertRaises(ValueError):
                    self.plan(data)

    def test_a_valid_plan_is_executable_on_its_resource_plan_alone(self):
        # decided_2026-10-17_emp-david: the volume is Longhorn on our own
        # hardware, so there is no bill to evidence. The cost gate and every
        # field it signed are gone; the exact plan hash alone reaches the patch.
        native=inputs()
        proposal=self.plan(native)
        for retired in ('executable','monetary_cost','resource_plan_sha256'):
            self.assertNotIn(retired,proposal)
        self.assertFalse(hasattr(MODULE,'cost_evidence'))
        writes=[]
        def patch(value):
            writes.append(value)
            native['pvc']['spec']['resources']['requests']['storage']='96Gi'
            native['pvc']['status']['capacity']['storage']='96Gi'
            native['pv']['spec']['capacity']['storage']='96Gi'
            native['volume']['spec']['size']=str(96*GIB)
            native['mount'].update(capacity_bytes=96*GIB,available_bytes=29*GIB)
        receipt=MODULE.execute(lambda: copy.deepcopy(native),patch,'96Gi',MODULE.digest(proposal))
        self.assertEqual(writes,[proposal['write']['patch']])
        self.assertEqual(receipt['plan_sha256'],MODULE.digest(proposal))

    def test_real_sized_native_evidence_reaches_its_one_patch(self):
        # One live read was a 951361-byte envelope (ops-request 7fb17ef5). The
        # 30000-byte pre-write budget predated the attach carrier (1200f7a7)
        # and refused every real patch; the carrier's own bounds now decide.
        with tempfile.TemporaryDirectory(prefix='dev-work-fixture-') as directory:
            probe=stubbed(directory,inputs(),noise=200)
            approved=MODULE.digest(self.plan(probe.read()))
            native=MODULE.Native(probe.kubectl,budget_seconds=120)
            receipt=MODULE.execute(native.read,native.patch,'96Gi',approved,room=native.room)
            self.assertGreater(len(native.evidence_bytes()),30000)
            self.assertEqual(receipt['mounted_available_bytes'],29*GIB)
            self.assertEqual([call[0] for call in calls(directory)].count('patch'),1)

    def test_a_write_starts_only_when_its_readback_fits_the_carrier(self):
        with tempfile.TemporaryDirectory(prefix='dev-work-fixture-') as directory:
            probe=stubbed(directory,inputs(),noise=50)
            approved=MODULE.digest(self.plan(probe.read()))
            per=probe.largest_read
            self.assertGreater(per,0)
            native=MODULE.Native(probe.kubectl,budget_seconds=120)
            # Two reads fit with a spare read's worth; a readback would not.
            native.carrier=types.SimpleNamespace(MAX_ORIGINAL=per*3+per//2,MAX_ENVELOPE=8*GIB,
                                                 record=native.carrier.record)
            with self.assertRaisesRegex(ValueError,'nothing patched'):
                MODULE.execute(native.read,native.patch,'96Gi',approved,room=native.room)
            self.assertNotIn('patch',[call[0] for call in calls(directory)])

    def test_readbacks_spread_across_the_wait_and_stop_inside_the_carrier(self):
        with tempfile.TemporaryDirectory(prefix='dev-work-fixture-') as directory:
            probe=stubbed(directory,inputs(),noise=50)
            approved=MODULE.digest(self.plan(probe.read()))
            per=probe.largest_read
            (Path(directory)/'pending').touch()
            native=MODULE.Native(probe.kubectl,budget_seconds=120)
            native.carrier=types.SimpleNamespace(MAX_ORIGINAL=per*6+per//2,MAX_ENVELOPE=8*GIB,
                                                 record=native.carrier.record)
            slept=[]
            # Only the planner's own clock: subprocess polls the shared time.sleep.
            clock=types.SimpleNamespace(monotonic=time.monotonic,time=time.time,sleep=slept.append)
            with unittest.mock.patch.object(MODULE,'time',clock), \
                    self.assertRaisesRegex(ValueError,'request stands'):
                MODULE.execute(native.read,native.patch,'96Gi',approved,wait_seconds=600,room=native.room)
            self.assertEqual([call[0] for call in calls(directory)].count('patch'),1)
            # Three readbacks fit after two plan reads and one spare: one at
            # once, then the window split between the two that remain.
            self.assertEqual(len(slept),2)
            self.assertAlmostEqual(slept[0],300,delta=5)
            # Each read gets the claim twice: the probe's, two plan reads, three readbacks.
            self.assertEqual(sum(call[:2]==['get','pvc'] for call in calls(directory)),2*(1+2+3))
            self.assertLessEqual(len(MODULE.render(native.records).encode()),native.carrier.MAX_ORIGINAL)

    def test_a_failed_upload_after_the_patch_never_denies_the_effect(self):
        # Review b9075eb9 probe A: the patch landed and the readback proved
        # growth, then the attach door failed. The record must say the request
        # stands and carry the receipt, never "no proposal/effect".
        request='7fb17ef5-1f8e-4d27-a373-fc8f85b4b67d'
        with tempfile.TemporaryDirectory(prefix='dev-work-fixture-') as directory:
            probe=stubbed(directory,inputs(),noise=200)
            approved=MODULE.digest(self.plan(probe.read()))
            real=MODULE.load_carrier()
            def unavailable(part):
                raise OSError('synthetic file service unavailable')
            carrier=types.SimpleNamespace(MAX_ORIGINAL=real.MAX_ORIGINAL,MAX_ENVELOPE=real.MAX_ENVELOPE,
                record=lambda raw,req,failure:real.deliver(raw,req,unavailable,failure))
            env={'BOSS_DEV_WORK_KUBECTL':' '.join(probe.kubectl),'OPS_REQUEST_ID':request}
            with unittest.mock.patch.object(MODULE,'load_carrier',lambda:carrier), \
                    unittest.mock.patch.dict(os.environ,env), self.assertRaises(ValueError) as refused:
                MODULE.main(['96Gi',approved])
            self.assertEqual([call[0] for call in calls(directory)].count('patch'),1)
        message=str(refused.exception)
        self.assertTrue(message.startswith('request stands; patch issued'),message)
        self.assertIn('"mounted_available_bytes": '+str(29*GIB),message)
        self.assertNotIn('no proposal/effect',message)

    def test_a_refusal_after_the_patch_says_the_request_stands(self):
        # Any later refusal, not only execute's own, follows an issued patch.
        native=MODULE.Native(['false'])
        native.records=[{'argv':['patch','pvc','boss-dev-work'],'exit':0,'stdout_b64':'','stderr_b64':''}]
        native.evidence=lambda failure=None:None
        with unittest.mock.patch.object(MODULE,'Native',lambda *args,**kwargs:native), \
                unittest.mock.patch.object(MODULE,'execute',unittest.mock.Mock(side_effect=ValueError('native read/write failed'))), \
                unittest.mock.patch.dict(os.environ,{'BOSS_DEV_WORK_KUBECTL':'unused-stub'}), \
                self.assertRaisesRegex(ValueError,'^request stands; patch issued; native read/write failed$'):
            MODULE.main(['96Gi','a'*64])

    def test_the_patch_budgets_its_own_record_before_any_readback(self):
        # Review b9075eb9 probe B: two reads plus a spare fit, but after the
        # patch's own returned record no readback did. Refuse before writing.
        with tempfile.TemporaryDirectory(prefix='dev-work-fixture-') as directory:
            probe=stubbed(directory,inputs(),noise=50)
            approved=MODULE.digest(self.plan(probe.read()))
            per=probe.largest_read
            native=MODULE.Native(probe.kubectl,budget_seconds=120)
            native.carrier=types.SimpleNamespace(MAX_ORIGINAL=per*4+512,MAX_ENVELOPE=8*GIB)
            with self.assertRaisesRegex(ValueError,'nothing patched'):
                MODULE.execute(native.read,native.patch,'96Gi',approved,room=native.room)
            self.assertNotIn('patch',[call[0] for call in calls(directory)])

    def test_ledger_requires_exactly_one_document(self):
        import subprocess
        for raw in ('', ' \n', '{}\n{}\n'):
            result = subprocess.run(['bash', str(ROOT/'infra/lib/check-longhorn-expansion.sh')],
                                    input=raw, text=True, capture_output=True)
            self.assertNotEqual(result.returncode, 0, raw)

    def test_wrapper_effect_only_follows_successful_mutating_child(self):
        import subprocess
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for path in ('forge','lib','estate','bin'):
                (root/path).mkdir()
            wrapper = root/'forge/dev-work-volume.sh'
            wrapper.write_bytes((ROOT/'infra/forge/dev-work-volume.sh').read_bytes())
            (root/'lib/sor.sh').write_text(':\n')
            (root/'estate/ops-credentials.sh').write_text('ops_image_ready() { :; }\nops_kubectl() { echo harmless-stub; }\n')
            child = root/'bin/python3'
            child.write_text('#!/bin/sh\nexit "${CHILD_STATUS}"\n')
            child.chmod(0o755)
            effect = 'expand-dev-work-volume: mounted capacity and session continuity observed; record conservation remains unproven'
            for args, status, expected in ((['--plan','96Gi'],0,False), (['96Gi','a'*64],78,False), (['96Gi','a'*64],0,True)):
                env = dict(os.environ, PATH=str(root/'bin')+':'+os.environ['PATH'], CHILD_STATUS=str(status))
                result = subprocess.run(['bash', str(wrapper), *args],env=env,text=True,capture_output=True)
                self.assertEqual(result.returncode,status)
                self.assertEqual(effect in result.stdout, expected, result.stdout)

    def plan(self, data=None, size='96Gi'):
        return MODULE.plan(inputs() if data is None else data, size)

    def test_positive_pins_fixed_identity_and_both_admission_checks(self):
        plan = self.plan()
        self.assertEqual(plan['claim']['uid'], 'uid-boss-dev-work')
        self.assertEqual(plan['target_bytes'], 96 * GIB)
        self.assertEqual(plan['incremental_replica_bytes'], 32 * GIB)
        self.assertEqual(plan['pod']['uid'], 'uid-boss-dev-running')
        self.assertTrue(plan['disks'])
        self.assertEqual(plan['disks'][0]['required_bytes'], 32 * GIB)
        self.assertEqual(plan['write']['patch'][0], {'op': 'test', 'path': '/metadata/uid', 'value': 'uid-boss-dev-work'})
        self.assertEqual(plan['write']['patch'][-1]['path'], '/spec/resources/requests/storage')

    def grown(self, gib, available_gib):
        # A claim already expanded to <gib>, every native layer agreeing.
        data = inputs()
        data['pvc']['spec']['resources']['requests']['storage'] = '%dGi' % gib
        data['pvc']['status']['capacity']['storage'] = '%dGi' % gib
        data['pv']['spec']['capacity']['storage'] = '%dGi' % gib
        data['volume']['spec']['size'] = str(gib * GIB)
        data['mount']['capacity_bytes'] = gib * GIB
        data['mount']['available_bytes'] = available_gib * GIB
        return data

    def test_the_plan_ceiling_alone_refuses_129Gi_from_a_96Gi_claim(self):
        # Review 2814c3c5 M3: from today's 64Gi claim the 2x bound refuses
        # 129Gi first, so nothing killed a wider _plan ceiling. From 96Gi the
        # 2x bound admits 192Gi and the 65..128 ceiling is the only refusal.
        data = self.grown(96, 40)
        self.assertEqual(self.plan(data, '128Gi')['target_bytes'], 128 * GIB)
        with self.assertRaisesRegex(ValueError, 'outside approved 65..128'):
            self.plan(data, '129Gi')

    def test_the_plan_floor_alone_refuses_64Gi_from_a_40Gi_claim(self):
        # Review 2814c3c5 M4: the 65GiB lower bound was redundant with
        # grow-only from 64Gi. From the 40Gi manifest floor 64Gi passes
        # grow-only, 2x and headroom, so only the range refuses it.
        data = self.grown(40, 5)
        with self.assertRaisesRegex(ValueError, 'outside approved 65..128'):
            self.plan(data, '64Gi')
        self.assertEqual(self.plan(data, '65Gi')['target_bytes'], 65 * GIB)

    def test_projected_mounted_headroom_alone_refuses_65Gi_with_5GiB_free(self):
        # Review 2814c3c5 M7: the 53Gi case that used to reach this guard is
        # now refused by the floor. 5GiB free + 1GiB growth projects 6GiB,
        # under the unchanged 18GiB floor; 18GiB free projects 19GiB.
        with self.assertRaisesRegex(ValueError, 'does not project mounted headroom'):
            self.plan(self.grown(64, 5), '65Gi')
        self.assertEqual(self.plan(self.grown(64, 18), '65Gi')['target_bytes'], 65 * GIB)

    def test_an_unexpected_diagnostic_error_never_replaces_the_plan_result(self):
        # Review 2814c3c5 advisory 5: the diagnostic claims never to affect
        # the plan, so no error inside it may escape main's finally block.
        native = MODULE.Native(['unused'])
        native.room = lambda: 2
        def malformed(*args, **kwargs):
            raise AttributeError('synthetic malformed native object')
        native.get = malformed
        result = native.worker_scheduling(inputs())
        self.assertEqual(result['state'], 'UNKNOWN')
        self.assertIn('synthetic malformed', result['cause'])
        empty = {'kind': 'List', 'metadata': {}, 'items': []}
        native.get = lambda *args, **kwargs: empty
        with unittest.mock.patch.object(MODULE, 'dev_build_scheduling',
                                        unittest.mock.Mock(side_effect=IndexError('synthetic index'))):
            self.assertIn('synthetic index', native.worker_scheduling(inputs())['cause'])
        with tempfile.TemporaryDirectory(prefix='dev-work-fixture-') as directory:
            probe = stubbed(directory, inputs())
            expected = MODULE.render(self.plan(probe.read()))
            env = {'BOSS_DEV_WORK_KUBECTL': ' '.join(probe.kubectl),
                   'OPS_REQUEST_ID': '7fb17ef5-1f8e-4d27-a373-fc8f85b4b67d'}
            out, err = io.StringIO(), io.StringIO()
            with unittest.mock.patch.object(MODULE, 'dev_build_scheduling',
                                            unittest.mock.Mock(side_effect=AttributeError('synthetic escape'))), \
                    unittest.mock.patch.dict(os.environ, env), \
                    contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                MODULE.main(['--plan', '96Gi'])
        self.assertEqual(out.getvalue(), expected)
        self.assertIn('synthetic escape', err.getvalue())

    def test_only_grows_with_explicit_cap(self):
        for size in ['64Gi', '39Gi', '53Gi', '129Gi', '256Gi', '064Gi', '64G', '64.0Gi', '-1Gi', '64Gi ignored']:
            with self.subTest(size=size), self.assertRaises(ValueError):
                self.plan(size=size)

        smaller = inputs()
        smaller['pvc']['spec']['resources']['requests']['storage'] = '40Gi'
        smaller['pvc']['status']['capacity']['storage'] = '40Gi'
        smaller['pv']['spec']['capacity']['storage'] = '40Gi'
        smaller['volume']['spec']['size'] = str(40 * GIB)
        smaller['mount']['capacity_bytes'] = 40 * GIB
        with self.assertRaisesRegex(ValueError, 'at most twice'):
            self.plan(smaller, '96Gi')

    def test_optional_empty_conditions_follow_native_api_semantics(self):
        data=inputs()
        del data['pvc']['status']['conditions']
        self.assertEqual(self.plan(data),self.plan())
        data['pods']['items'].append(obj('Pod','unrelated','boss-dev',spec={'containers':[{'name':'other'}]},status={'phase':'Pending'}))
        self.assertEqual(self.plan(data),self.plan())

    def test_native_unknowns_refuse(self):
        for key in inputs():
            data = inputs()
            del data[key]
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.plan(data)

    def test_approved_execution_uses_one_conditional_patch_and_checks_mounted_result(self):
        native = inputs()
        changes=[]
        expected=MODULE.digest(self.plan(native))
        def read():
            return copy.deepcopy(native)
        def patch(value):
            changes.append(value)
            native['pvc']['spec']['resources']['requests']['storage']='96Gi'
            native['pvc']['status']['capacity']['storage']='96Gi'
            native['pv']['spec']['capacity']['storage']='96Gi'
            native['volume']['spec']['size']=str(96*GIB)
            native['mount']['available_bytes']=29*GIB
            native['mount']['capacity_bytes']=96*GIB
        receipt=MODULE.execute(read,patch,'96Gi',expected)
        self.assertEqual(len(changes),1)
        self.assertEqual(changes[0][0]['path'],'/metadata/uid')
        self.assertEqual(receipt['mounted_available_bytes'],29*GIB)
        self.assertEqual(receipt['record_conservation'],'unproven; independent evidence required')

    def test_plan_drift_and_incomplete_effect_never_claim_success_or_retry_patch(self):
        for variant in ['approval','uid','pending','pod-restart','floor','volume-binding','container-start','filesystem-not-grown']:
            native=inputs()
            approved=MODULE.digest(self.plan(native))
            changes=[]
            if variant=='approval':
                approved='0'*64
            if variant=='uid':
                native['pvc']['metadata']['uid']='replacement'
            def read():
                return copy.deepcopy(native)
            def patch(value):
                changes.append(value)
                if variant!='pending':
                    native['pvc']['spec']['resources']['requests']['storage']='96Gi'
                    native['pvc']['status']['capacity']['storage']='96Gi'
                    native['pv']['spec']['capacity']['storage']='96Gi'
                    native['volume']['spec']['size']=str(96*GIB)
                    native['mount']['available_bytes']=29*GIB
                    if variant!='filesystem-not-grown':
                        native['mount']['capacity_bytes']=96*GIB
                if variant=='pod-restart':
                    native['pods']['items'][0]['metadata']['uid']='new-pod'
                if variant=='floor':
                    native['mount']['available_bytes']=18*GIB
                if variant=='volume-binding':
                    native['pvc']['spec']['volumeName']='pvc-other'
                if variant=='container-start':
                    native['pods']['items'][0]['status']['containerStatuses'][0]['state']['running']['startedAt']='2026-10-15T15:00:00Z'
            with self.subTest(variant=variant),self.assertRaises(ValueError):
                MODULE.execute(read,patch,'96Gi',approved)
            self.assertLessEqual(len(changes),1)
            if variant in ['approval','uid']:
                self.assertEqual(changes,[])

    def test_native_json_duplicate_keys_and_partial_lists_are_not_evidence(self):
        with self.assertRaises(ValueError):
            MODULE.decode_native(b'{"kind":"List","kind":"wrong"}')
        data=inputs()
        data['replicas']['metadata']['remainingItemCount']=1
        with self.assertRaises(ValueError):
            self.plan(data)

    def test_whole_original_streams_survive_compression_and_large_inputs_refuse_before_patch(self):
        native=MODULE.Native(['false'])
        original=os.urandom(100000)
        native.records=[{'exit':0,'stdout_b64':base64.b64encode(original).decode(),'stderr_b64':''}]
        encoded=json.loads(native.evidence_bytes())
        raw=gzip.decompress(base64.b64decode(encoded['data']))
        self.assertEqual(len(raw),encoded['original_bytes'])
        self.assertEqual(hashlib.sha256(raw).hexdigest(),encoded['original_sha256'])
        self.assertEqual(base64.b64decode(json.loads(raw)[0]['stdout_b64']),original)
        # A read of this size leaves the real carrier no room for its readback.
        native.largest_read=len(raw)
        native.carrier=types.SimpleNamespace(MAX_ORIGINAL=2*len(raw),MAX_ENVELOPE=native.carrier.MAX_ENVELOPE)
        with self.assertRaisesRegex(ValueError,'nothing patched'):
            native.patch([])
        self.assertEqual(len(native.records),1,'no native write command may start')

    def test_native_deadline_retains_real_partial_streams_and_starts_no_later_read(self):
        native=MODULE.Native([os.sys.executable,'-c',
            'import os,time;os.write(1,b"partial\\x00\\n");os.write(2,b"diagnostic\\n");time.sleep(5)'],budget_seconds=0.1)
        with self.assertRaisesRegex(ValueError,'timed out'):
            native.call([])
        self.assertEqual(len(native.records),1)
        record=native.records[0]
        self.assertIsNone(record['exit'])
        self.assertEqual(base64.b64decode(record['stdout_b64']),b'partial\x00\n')
        self.assertEqual(base64.b64decode(record['stderr_b64']),b'diagnostic\n')
        with self.assertRaisesRegex(ValueError,'deadline'):
            native.call([])
        self.assertEqual(len(native.records),1)

    def test_native_reader_uses_fixed_resources_and_preserves_original_streams(self):
        with tempfile.TemporaryDirectory(prefix='dev-work-fixture-') as directory:
            native=stubbed(directory,inputs())
            observed=native.read()
            self.assertEqual(self.plan(observed),self.plan())
            self.assertTrue(native.records)
            self.assertTrue(all('stdout_b64' in record and 'stderr_b64' in record and record['exit']==0 for record in native.records))
            self.assertTrue(all(call[0] in ['get','auth','exec'] for call in calls(directory)))
            (Path(directory)/'replace-pod').touch()
            replaced=MODULE.Native(native.kubectl)
            with self.assertRaisesRegex(ValueError,'changed during'):
                replaced.read()

    def test_converge_preserves_live_growth_and_all_other_manifest_bytes(self):
        source=(ROOT/'infra/cluster/manifests/boss-dev.yaml').read_text()
        grown=inputs()['pvc']
        grown['spec']['resources']['requests']['storage']='96Gi'
        rendered=MODULE.render_manifest(source,grown)
        self.assertEqual(rendered,source.replace('      storage: 40Gi\n','      storage: 96Gi\n',1))
        for value in ['40Gi', '64Gi', '65Gi', '96Gi', '128Gi']:
            live = copy.deepcopy(grown)
            live['spec']['resources']['requests']['storage'] = value
            self.assertEqual(MODULE.render_manifest(source, live),
                             source.replace('      storage: 40Gi\n', '      storage: '+value+'\n', 1))
        self.assertEqual(MODULE.render_manifest(source,None),source)
        for value in ['39Gi','129Gi','256Gi']:
            bad=copy.deepcopy(grown)
            bad['spec']['resources']['requests']['storage']=value
            with self.subTest(value=value), self.assertRaises(ValueError):
                MODULE.render_manifest(source,bad)
        runner=(ROOT/'infra/forge/cluster-deploy-runner.sh').read_text()
        self.assertLess(runner.index('--render-converge'),runner.index('deposit_config_apply '))

    def test_registry_requires_davids_exact_plan_and_no_target_arguments(self):
        directory=ROOT/'infra/ops/verbs'
        planner=json.loads((directory/'plan-dev-work-volume-expansion.json').read_text())
        actuator=json.loads((directory/'expand-dev-work-volume.json').read_text())
        self.assertEqual(planner['argv'],['infra/forge/dev-work-volume.sh','--plan','{1}'])
        self.assertEqual(actuator['argv'],['infra/forge/dev-work-volume.sh','{1}','{2}'])
        self.assertEqual([v['name'] for v in planner['params']],['size'])
        self.assertEqual([v['name'] for v in actuator['params']],['size','plan_sha256'])
        self.assertTrue(actuator['requires_approval'])
        self.assertEqual(actuator['approvers'],['emp-david'])
        self.assertEqual(actuator['plan_verb'],'plan-dev-work-volume-expansion')
        # The cost prerequisite was dropped by decided_2026-10-17_emp-david;
        # neither row may still promise a billing reader it no longer waits on.
        for row in (planner,actuator):
            self.assertNotIn('billing',row['about'])
            self.assertNotIn('monetary',row['about'])
            self.assertIn('decided_2026-10-17_emp-david',row['about'])
        self.assertEqual(planner['capture'],'separate-streams')
        self.assertEqual(actuator['capture'],'separate-streams')
        self.assertEqual(planner['params'][0]['pattern'], actuator['params'][0]['pattern'])
        for amount in range(1, 161):
            self.assertEqual(bool(__import__('re').fullmatch(planner['params'][0]['pattern'], str(amount)+'Gi')),
                             65 <= amount <= 128)
        for value in ['0Gi','64Gi','129Gi','256Gi','064Gi']:
            self.assertIsNone(__import__('re').fullmatch(planner['params'][0]['pattern'],value))

    def test_identity_resize_health_and_roster_refusals(self):
        changes = [
            ('pvc', ['metadata', 'namespace'], 'boss'),
            ('pvc', ['metadata', 'uid'], ''),
            ('pvc', ['metadata', 'deletionTimestamp'], '2026-10-15T12:00:00Z'),
            ('pvc', ['status', 'capacity', 'storage'], '39Gi'),
            ('pvc', ['status', 'conditions'], [{'type': 'FileSystemResizePending', 'status': 'True'}]),
            ('pv', ['spec', 'claimRef', 'uid'], 'another'),
            ('sc', ['allowVolumeExpansion'], False),
            ('volume', ['status', 'robustness'], 'degraded'),
            ('volume', ['spec', 'size'], str(39 * GIB)),
            ('volume', ['spec', 'numberOfReplicas'], 2),
            ('replicas', ['metadata', 'continue'], 'more'),
            ('nodes', ['metadata', 'continue'], 'more'),
            ('pods', ['items'], []),
        ]
        for name, path, value in changes:
            data = inputs()
            target = data[name]
            for key in path[:-1]:
                target = target[key]
            target[path[-1]] = value
            with self.subTest(change=(name, path)), self.assertRaises(ValueError):
                self.plan(data)

    def test_live_free_space_is_rechecked_but_not_signed(self):
        first = self.plan()
        data = inputs()
        data['nodes']['items'][0]['status']['diskStatus']['disk']['storageAvailable'] -= GIB
        self.assertEqual(first, self.plan(data))
        data['nodes']['items'][0]['status']['diskStatus']['disk']['storageAvailable'] = GIB
        with self.assertRaises(ValueError):
            self.plan(data)

    def live_moved(self, data, scheduled_gib=100, maximum_gib=501):
        # What a dev-build worker volume's creation or retirement and a new
        # ssh/tmux login change between render and approval (4f60b5b2:
        # requests 06873bac and 20c62983 differed in exactly these bytes).
        disk = data['nodes']['items'][0]['status']['diskStatus']['disk']
        disk['storageScheduled'] = scheduled_gib * GIB
        disk['storageMaximum'] = maximum_gib * GIB
        data['mount']['processes'] = data['mount']['processes'] + [{'pid': 830158, 'starttime': 243558487, 'name': 'sshd'}]
        return data

    def test_live_scheduling_and_session_figures_are_recorded_but_never_signed(self):
        # 4f60b5b2: the signed bytes carry stable identity, the growth and the
        # admission verdict; the live figures ride beside them, unhashed.
        first = MODULE.render(self.plan())
        moved = self.live_moved(inputs())
        self.assertEqual(MODULE.render(self.plan(moved)), first, 'one plan hash across live scheduling moves')
        # (the minimal_available SETTING is configuration, and stays signed)
        for live in ('scheduled', 'storageMaximum', '"max"', '"reserved"', '"provisioned_limit"',
                     'available_bytes', 'processes', 'sshd', 'codex'):
            self.assertNotIn(live, first, live)
        verdicts = self.plan()['disks']
        self.assertEqual([(d['uuid'], d['count'], d['required_bytes'], d['verdict']) for d in verdicts],
                         [('disk-one', 1, 32 * GIB, 'admitted')])
        self.assertIn('ValidateDiskAvailableForExpansion', verdicts[0]['rule'])
        # A stable fact still changes the signed bytes.
        setting = inputs()
        setting['overprovisioning']['value'] = '200'
        self.assertNotEqual(MODULE.render(self.plan(setting)), first)
        # The live figures are printed beside the plan, never inside it.
        with tempfile.TemporaryDirectory(prefix='dev-work-fixture-') as directory:
            probe = stubbed(directory, moved)
            env = {'BOSS_DEV_WORK_KUBECTL': ' '.join(probe.kubectl),
                   'OPS_REQUEST_ID': '7fb17ef5-1f8e-4d27-a373-fc8f85b4b67d'}
            out, err = io.StringIO(), io.StringIO()
            with unittest.mock.patch.dict(os.environ, env), \
                    contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                MODULE.main(['--plan', '96Gi'])
        self.assertEqual(out.getvalue(), first)
        lines = [line for line in err.getvalue().splitlines() if line.startswith('dev-work-live: ')]
        self.assertEqual(len(lines), 1, err.getvalue())
        live = json.loads(lines[0][len('dev-work-live: '):])
        self.assertEqual(live['disks'][0]['scheduled'], 100 * GIB)
        self.assertEqual(live['disks'][0]['scheduled_after'], 132 * GIB)
        self.assertIn('sshd', [p['name'] for p in live['mount']['processes']])

    def test_an_approval_survives_a_live_scheduling_move_that_still_admits(self):
        native = inputs()
        approved = MODULE.digest(self.plan(native))
        self.live_moved(native)
        writes = []
        def patch(value):
            writes.append(value)
            native['pvc']['spec']['resources']['requests']['storage'] = '96Gi'
            native['pvc']['status']['capacity']['storage'] = '96Gi'
            native['pv']['spec']['capacity']['storage'] = '96Gi'
            native['volume']['spec']['size'] = str(96 * GIB)
            native['mount'].update(capacity_bytes=96 * GIB, available_bytes=29 * GIB)
        receipt = MODULE.execute(lambda: copy.deepcopy(native), patch, '96Gi', approved)
        self.assertEqual(len(writes), 1)
        self.assertEqual(receipt['plan_sha256'], approved)
        # The figures admission judged at the write are in the effect's record.
        self.assertEqual(receipt['admitted_live']['disks'][0]['scheduled'], 100 * GIB)

    def test_an_admission_that_flips_to_refused_refuses_at_execute_with_live_figures(self):
        # Approved at one ledger; the first execute read moved but admits (the
        # signed verdict holds); the read immediately before the patch is
        # refused. The live re-check refuses, naming the figures, and nothing
        # is patched.
        native = inputs()
        approved = MODULE.digest(self.plan(native))
        admitted = self.live_moved(copy.deepcopy(native))
        refused = copy.deepcopy(admitted)
        refused['nodes']['items'][0]['status']['diskStatus']['disk']['storageScheduled'] = 480 * GIB
        reads = [admitted, refused]
        writes = []
        with self.assertRaisesRegex(ValueError, 'Longhorn admission cannot answer or refuses: .*ScheduledTotal'):
            MODULE.execute(lambda: copy.deepcopy(reads.pop(0)), writes.append, '96Gi', approved)
        self.assertEqual(reads, [])
        self.assertEqual(writes, [])

    def test_session_continuity_is_judged_from_the_read_before_the_write(self):
        # The process list is no longer signed, so a login after approval is
        # admitted; a baseline process lost after the patch still means the
        # request stands with its effect unproven.
        native = inputs()
        approved = MODULE.digest(self.plan(native))
        self.live_moved(native)
        writes = []
        def patch(value):
            writes.append(value)
            native['pvc']['spec']['resources']['requests']['storage'] = '96Gi'
            native['pvc']['status']['capacity']['storage'] = '96Gi'
            native['pv']['spec']['capacity']['storage'] = '96Gi'
            native['volume']['spec']['size'] = str(96 * GIB)
            native['mount'].update(capacity_bytes=96 * GIB, available_bytes=29 * GIB)
            native['mount']['processes'] = native['mount']['processes'][:1]
        with self.assertRaisesRegex(ValueError, 'request stands; mount/session continuity changed'):
            MODULE.execute(lambda: copy.deepcopy(native), patch, '96Gi', approved)
        self.assertEqual(len(writes), 1)

    def test_each_replica_on_one_disk_pays_for_growth_and_each_bound_can_refuse(self):
        data=inputs()
        second=copy.deepcopy(data['replicas']['items'][0])
        second['metadata'].update(name='replica-two',uid='uid-replica-two')
        data['replicas']['items'].append(second)
        data['volume']['spec']['numberOfReplicas']=2
        planned=self.plan(data)
        self.assertEqual(planned['disks'][0]['required_bytes'],64*GIB)
        self.assertEqual(planned['incremental_replica_bytes'],64*GIB)
        data['nodes']['items'][0]['status']['diskStatus']['disk']['storageAvailable']=175*GIB
        with self.assertRaisesRegex(ValueError,'physical'):
            self.plan(data)
        single=inputs()
        single['nodes']['items'][0]['status']['diskStatus']['disk']['storageAvailable']=175*GIB
        self.plan(single)
        single['nodes']['items'][0]['status']['diskStatus']['disk']['storageScheduled']=480*GIB
        with self.assertRaisesRegex(ValueError,'Scheduling'):
            self.plan(single)

    def test_duplicate_and_malformed_integer_evidence_refuses(self):
        for key in ['replicas', 'nodes', 'pods']:
            data = inputs()
            data[key]['items'] *= 2
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.plan(data)

    def test_online_support_requires_actual_supported_runtime_and_mount(self):
        for key, path, value in [
            ('mount', ['filesystem'], 'nfs'),
            ('mount', ['source'], '/dev/longhorn/other'),
            ('mount', ['processes'], []),
            ('rights', ['patch_claim'], False),
            ('rights', ['read_mount'], False),
            ('volume', ['status', 'currentImage'], 'longhornio/longhorn-engine:v1.3.0'),
            ('csi_pods', ['items'], []),
        ]:
            data=inputs()
            target=data[key]
            for part in path[:-1]:
                target=target[part]
            target[path[-1]]=value
            with self.subTest(change=(key,path)), self.assertRaises(ValueError):
                self.plan(data)
        for value in [-1, 0.5, True, '1e10', 2**53]:
            data = inputs()
            data['nodes']['items'][0]['status']['diskStatus']['disk']['storageMaximum'] = value
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.plan(data)


if __name__ == '__main__':
    unittest.main()
