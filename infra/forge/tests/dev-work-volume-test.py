"""Fixed-purpose dev work expansion: native-shaped inputs and real shared ledger."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import unittest
import tempfile
import base64
import gzip
import hashlib
import time

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
                         'resources': {'requests': {'storage': '40Gi'}}},
                   status={'phase': 'Bound', 'capacity': {'storage': '40Gi'}, 'conditions': []}),
        'pv': obj('PersistentVolume', volume, spec={'claimRef': {'namespace': 'boss-dev', 'name': 'boss-dev-work', 'uid': 'uid-boss-dev-work'},
                  'csi': {'driver': 'driver.longhorn.io', 'volumeHandle': volume, 'fsType': 'ext4'},
                  'capacity': {'storage': '40Gi'}}),
        'sc': obj('StorageClass', 'longhorn-dev-disposable', provisioner='driver.longhorn.io', allowVolumeExpansion=True),
        'volume': obj('Volume', volume, 'longhorn-system', spec={'size': str(40 * GIB), 'numberOfReplicas': 1,
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
        'device': '8:16', 'available_bytes': 5*GIB, 'capacity_bytes': 40*GIB, 'boot_id': 'boot-id',
        'processes': [{'pid': 123, 'starttime': 1234, 'name': 'codex'}]}
    result['rights'] = {'patch_claim': True, 'read_mount': True}
    return result


def priced_inputs(amount='0'):
    """Synthetic authoritative read port only; Native has no billing adapter."""
    native=inputs()
    proposal=MODULE.plan(native,'64Gi')
    now=int(time.time())
    native['monetary_cost_evidence']={'state':'known','authority':'synthetic-test-billing-reader',
        'reference':'synthetic-original-receipt-1','observed_at':now,'valid_until':now+300,
        'resource_plan_sha256':proposal['resource_plan_sha256'], 'amount':amount,
        'currency':'USD','billing_period':'synthetic explicitly supplied billing period'}
    return native


class Planning(unittest.TestCase):
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
                    self.assertFalse(result['executable'])
                    self.assertEqual(result['monetary_cost']['state'], 'unknown')

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

    def test_unknown_cost_cannot_authorize_even_its_exact_signed_plan(self):
        native=inputs()
        proposal=self.plan(native)
        writes=[]
        with self.assertRaisesRegex(ValueError,'cost'):
            MODULE.execute(lambda: copy.deepcopy(native),writes.append,'64Gi',MODULE.digest(proposal))
        self.assertEqual(writes,[])

    def test_cost_evidence_must_be_well_formed_fresh_and_bound_to_entire_resource_plan(self):
        for evidence in ({}, {'state':'unknown'}, {'state':'known','amount':'0'},
                         {'state':'known','observed_at':0,'valid_until':1}):
            native=inputs()
            native['monetary_cost_evidence']=evidence
            writes=[]
            with self.subTest(evidence=evidence),self.assertRaisesRegex(ValueError,'cost'):
                MODULE.execute(lambda: copy.deepcopy(native),writes.append,'64Gi','a'*64)
            self.assertEqual(writes,[])

    def test_supplied_cost_originals_are_signed_without_inventing_zero_or_period(self):
        for amount in ('0','12.345678'):
            native=priced_inputs(amount)
            proposal=self.plan(native)
            self.assertTrue(proposal['executable'])
            self.assertEqual(proposal['monetary_cost'],native['monetary_cost_evidence'])
            changed=copy.deepcopy(native)
            changed['monetary_cost_evidence']['reference']='different-original'
            self.assertNotEqual(MODULE.digest(proposal),MODULE.digest(self.plan(changed)))
        self.assertFalse(self.plan()['executable'])

    def test_expired_malformed_and_cross_plan_cost_cannot_reach_patch(self):
        for key,value in (('amount','-1'),('amount',0),('amount','NaN'),('currency',''),
                          ('billing_period',''),('authority',''),('reference',''),
                          ('observed_at',True),('observed_at',int(time.time())+60),
                          ('valid_until',int(time.time())-1),('valid_until',int(time.time())+10000),
                          ('resource_plan_sha256','0'*64)):
            native=priced_inputs()
            approved=MODULE.digest(self.plan(native))
            native['monetary_cost_evidence'][key]=value
            writes=[]
            with self.subTest(key=key,value=value),self.assertRaisesRegex(ValueError,'cost'):
                MODULE.execute(lambda:copy.deepcopy(native),writes.append,'64Gi',approved)
            self.assertEqual(writes,[])
        for edit in ('claim','target','replicas'):
            native=priced_inputs()
            if edit=='claim':
                native['pvc']['metadata']['uid']='replacement'
                native['pv']['spec']['claimRef']['uid']='replacement'
            elif edit=='target':
                pass
            else:
                native['replicas']['items'][0]['metadata']['uid']='different-replica'
            with self.subTest(edit=edit),self.assertRaisesRegex(ValueError,'cost'):
                self.plan(native,'63Gi' if edit=='target' else '64Gi')

    def test_cost_rechecked_immediately_before_patch(self):
        for change in ('unknown','expired','different-cost'):
            native=priced_inputs()
            approved=MODULE.digest(self.plan(native))
            reads=[]
            writes=[]
            def read():
                value=copy.deepcopy(native)
                if reads:
                    if change=='unknown': value['monetary_cost_evidence']=None
                    elif change=='expired': value['monetary_cost_evidence']['valid_until']=1
                    else: value['monetary_cost_evidence']['amount']='1'
                reads.append(value)
                return value
            with self.subTest(change=change),self.assertRaises(ValueError):
                MODULE.execute(read,writes.append,'64Gi',approved)
            self.assertEqual(writes,[])

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
            for args, status, expected in ((['--plan','64Gi'],0,False), (['64Gi','a'*64],78,False), (['64Gi','a'*64],0,True)):
                env = dict(os.environ, PATH=str(root/'bin')+':'+os.environ['PATH'], CHILD_STATUS=str(status))
                result = subprocess.run(['bash', str(wrapper), *args],env=env,text=True,capture_output=True)
                self.assertEqual(result.returncode,status)
                self.assertEqual(effect in result.stdout, expected, result.stdout)

    def plan(self, data=None, size='64Gi'):
        return MODULE.plan(inputs() if data is None else data, size)

    def test_positive_pins_fixed_identity_and_both_admission_checks(self):
        plan = self.plan()
        self.assertEqual(plan['claim']['uid'], 'uid-boss-dev-work')
        self.assertEqual(plan['target_bytes'], 64 * GIB)
        self.assertEqual(plan['incremental_replica_bytes'], 24 * GIB)
        self.assertEqual(plan['pod']['uid'], 'uid-boss-dev-running')
        self.assertTrue(plan['disks'])
        self.assertEqual(plan['disks'][0]['required_bytes'], 24 * GIB)
        self.assertEqual(plan['write']['patch'][0], {'op': 'test', 'path': '/metadata/uid', 'value': 'uid-boss-dev-work'})
        self.assertEqual(plan['write']['patch'][-1]['path'], '/spec/resources/requests/storage')

    def test_only_grows_with_explicit_cap(self):
        for size in ['40Gi', '39Gi', '53Gi', '65Gi', '128Gi', '064Gi', '64G', '64.0Gi', '-1Gi', '64Gi ignored']:
            with self.subTest(size=size), self.assertRaises(ValueError):
                self.plan(size=size)

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
        native = priced_inputs()
        changes=[]
        expected=MODULE.digest(self.plan(native))
        def read():
            return copy.deepcopy(native)
        def patch(value):
            changes.append(value)
            native['pvc']['spec']['resources']['requests']['storage']='64Gi'
            native['pvc']['status']['capacity']['storage']='64Gi'
            native['pv']['spec']['capacity']['storage']='64Gi'
            native['volume']['spec']['size']=str(64*GIB)
            native['mount']['available_bytes']=29*GIB
            native['mount']['capacity_bytes']=64*GIB
        receipt=MODULE.execute(read,patch,'64Gi',expected)
        self.assertEqual(len(changes),1)
        self.assertEqual(changes[0][0]['path'],'/metadata/uid')
        self.assertEqual(receipt['mounted_available_bytes'],29*GIB)
        self.assertEqual(receipt['record_conservation'],'unproven; independent evidence required')

    def test_plan_drift_and_incomplete_effect_never_claim_success_or_retry_patch(self):
        for variant in ['approval','uid','pending','pod-restart','floor','volume-binding','container-start','filesystem-not-grown']:
            native=priced_inputs()
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
                    native['pvc']['spec']['resources']['requests']['storage']='64Gi'
                    native['pvc']['status']['capacity']['storage']='64Gi'
                    native['pv']['spec']['capacity']['storage']='64Gi'
                    native['volume']['spec']['size']=str(64*GIB)
                    native['mount']['available_bytes']=29*GIB
                    if variant!='filesystem-not-grown':
                        native['mount']['capacity_bytes']=64*GIB
                if variant=='pod-restart':
                    native['pods']['items'][0]['metadata']['uid']='new-pod'
                if variant=='floor':
                    native['mount']['available_bytes']=18*GIB
                if variant=='volume-binding':
                    native['pvc']['spec']['volumeName']='pvc-other'
                if variant=='container-start':
                    native['pods']['items'][0]['status']['containerStatuses'][0]['state']['running']['startedAt']='2026-10-15T15:00:00Z'
            with self.subTest(variant=variant),self.assertRaises(ValueError):
                MODULE.execute(read,patch,'64Gi',approved)
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
        with self.assertRaisesRegex(ValueError,'capture budget'):
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
            base=Path(directory)
            (base/'data.json').write_text(json.dumps(inputs()))
            stub=base/'kubectl.py'
            stub.write_text('''import json,sys
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
mapping={'pvc':'pvc','pv':'pv','storageclass':'sc','volumes.longhorn.io':'volume',
         'replicas.longhorn.io':'replicas','nodes.longhorn.io':'nodes'}
if a[:2]==['get','pod']:print(json.dumps(d['pods']['items'][0]));sys.exit(0)
if a[:2]==['get','pods']:key='pods' if a[a.index('-n')+1]=='boss-dev' else 'csi_pods'
elif a[:2]==['get','settings.longhorn.io']:key='overprovisioning' if a[2]=='storage-over-provisioning-percentage' else 'minimal_available'
elif a[0]=='get' and a[1] in mapping:key=mapping[a[1]]
else:raise RuntimeError('unexpected native command '+repr(a))
print(json.dumps(d[key]))
''')
            native=MODULE.Native([os.sys.executable,str(stub)])
            observed=native.read()
            self.assertIsNone(observed['monetary_cost_evidence'])
            self.assertEqual(self.plan(observed),self.plan())
            proposal=self.plan(observed)
            self.assertFalse(proposal['executable'])
            writes=[]
            with self.assertRaisesRegex(ValueError,'cost'):
                MODULE.execute(native.read,writes.append,'64Gi',MODULE.digest(proposal))
            self.assertEqual(writes,[])
            self.assertTrue(native.records)
            self.assertTrue(all('stdout_b64' in record and 'stderr_b64' in record and record['exit']==0 for record in native.records))
            calls=[json.loads(line) for line in (base/'calls').read_text().splitlines()]
            self.assertTrue(all(call[0] in ['get','auth','exec'] for call in calls))
            (base/'replace-pod').touch()
            replaced=MODULE.Native([os.sys.executable,str(stub)])
            with self.assertRaisesRegex(ValueError,'changed during'):
                replaced.read()

    def test_converge_preserves_live_growth_and_all_other_manifest_bytes(self):
        source=(ROOT/'infra/cluster/manifests/boss-dev.yaml').read_text()
        grown=inputs()['pvc']
        grown['spec']['resources']['requests']['storage']='64Gi'
        rendered=MODULE.render_manifest(source,grown)
        self.assertEqual(rendered,source.replace('      storage: 40Gi\n','      storage: 64Gi\n',1))
        self.assertEqual(MODULE.render_manifest(source,None),source)
        for value in ['39Gi','65Gi','128Gi']:
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
        self.assertIn('no authoritative billing adapter',planner['about'])
        self.assertIn('no authoritative billing adapter',actuator['about'])
        self.assertEqual(planner['capture'],'separate-streams')
        self.assertEqual(actuator['capture'],'separate-streams')
        for value in ['0Gi','40Gi','65Gi','128Gi','064Gi']:
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

    def test_each_replica_on_one_disk_pays_for_growth_and_each_bound_can_refuse(self):
        data=inputs()
        second=copy.deepcopy(data['replicas']['items'][0])
        second['metadata'].update(name='replica-two',uid='uid-replica-two')
        data['replicas']['items'].append(second)
        data['volume']['spec']['numberOfReplicas']=2
        planned=self.plan(data)
        self.assertEqual(planned['disks'][0]['required_bytes'],48*GIB)
        self.assertEqual(planned['incremental_replica_bytes'],48*GIB)
        data['nodes']['items'][0]['status']['diskStatus']['disk']['storageAvailable']=150*GIB
        with self.assertRaisesRegex(ValueError,'physical'):
            self.plan(data)
        single=inputs()
        single['nodes']['items'][0]['status']['diskStatus']['disk']['storageAvailable']=150*GIB
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
