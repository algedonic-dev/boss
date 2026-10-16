#!/usr/bin/env python3
"""Fixed dev work purpose (31d76e37 / accepted design 9687bcaf).

The pure planner consumes native observations. It does not execute a resize.
Longhorn's arithmetic stays in its shared checked ledger, never copied here.
"""
import json
import hashlib
import base64
import gzip
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import importlib.util

GIB = 1073741824
TARGET = ('boss-dev', 'boss-dev-work')
ROOT = Path(__file__).resolve().parents[2]


def need(condition, message):
    if not condition:
        raise ValueError(message)


def decode_native(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            need(key not in result, 'native JSON repeats an object key')
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=unique)


def uint(value):
    need(type(value) in (str, int), 'integer is unavailable or malformed')
    need(re.fullmatch(r'[0-9]+', str(value)) is not None, 'nonnegative integer required')
    result = int(value)
    need(result <= 9007199254740991, 'integer exceeds exact ledger range')
    return result


def size(value):
    need(isinstance(value, str), 'storage size is unavailable')
    match = re.fullmatch(r'([1-9][0-9]*)(Gi|Mi|Ki)?', value)
    need(match is not None, 'storage quantity must be exact binary bytes')
    return uint(match[1]) * {None: 1, 'Ki': 1024, 'Mi': 1048576, 'Gi': GIB}[match[2]]


def identity(value, kind, name=None, namespace=None):
    need(type(value) is dict and value.get('kind') == kind, 'typed native object missing: ' + kind)
    expected = ('longhorn.io/v1beta2' if kind in ('Volume','Replica','Node','Setting') else
                'storage.k8s.io/v1' if kind=='StorageClass' else 'v1')
    need(value.get('apiVersion') == expected, 'native API version is unsupported')
    meta = value.get('metadata', {})
    need(type(meta) is dict and isinstance(meta.get('name'), str) and meta['name'], 'object name missing')
    need(isinstance(meta.get('uid'), str) and meta['uid'], 'object UID missing')
    need(not meta.get('deletionTimestamp'), 'object is deleting')
    need(name is None or meta['name'] == name, 'native object name mismatch')
    need(namespace is None or meta.get('namespace') == namespace, 'native object namespace mismatch')
    need(kind not in ('PersistentVolume','StorageClass') or not meta.get('namespace'), 'cluster object has a namespace')
    return {'name': meta['name'], 'uid': meta['uid']}


def whole(value, kind, namespace):
    need(type(value) is dict and value.get('kind') in ('List', kind + 'List'), 'typed complete list missing')
    metadata = value.get('metadata', {})
    need(type(metadata) is dict and not metadata.get('continue')
         and metadata.get('remainingItemCount') in (None, 0), 'native list is partial')
    rows = value.get('items')
    need(type(rows) is list, 'native list items missing')
    names, uids = set(), set()
    for row in rows:
        ident = identity(row, kind, namespace=namespace)
        need(ident['name'] not in names and ident['uid'] not in uids, 'native list identity duplicated')
        names.add(ident['name'])
        uids.add(ident['uid'])
    return rows


def qualified_longhorn_image(value, component):
    # Docker's familiar longhornio/name and docker.io/longhornio/name
    # denote the same repository (distribution/reference v0.6.0,
    # normalize.go: splitDockerDomain). No other registry, alias, tag or
    # digest is qualified by this version-specific capability check.
    familiar = 'longhornio/longhorn-' + component + ':v1.11.3'
    return value in (familiar, 'docker.io/' + familiar)


def plan(reads, requested):
    """Return stable signed facts only after live physical admission succeeds."""
    try:
        return _plan(reads, requested)
    except (KeyError, TypeError, IndexError) as error:
        raise ValueError('native observation incomplete: ' + str(error)) from error


def _plan(r, requested):
    need(type(requested) is str and re.fullmatch(r'[1-9][0-9]?Gi', requested) is not None,
         'explicit whole GiB target required')
    wanted = size(requested)
    need(wanted <= 64 * GIB, 'target exceeds approved 64 GiB ceiling')
    pvc = r['pvc']
    claim = identity(pvc, 'PersistentVolumeClaim', TARGET[1], TARGET[0])
    need(pvc['status']['phase'] == 'Bound', 'claim is not Bound')
    need(pvc['status'].get('conditions', []) == [], 'claim has conditions or unknown resize state')
    before = size(pvc['spec']['resources']['requests']['storage'])
    need(before == size(pvc['status']['capacity']['storage']), 'resize already in flight')
    need(before < wanted <= 2 * before, 'growth must be positive and at most twice current size')
    need(pvc['spec']['volumeMode'] == 'Filesystem' and pvc['spec']['accessModes'] == ['ReadWriteOnce'],
         'only the existing RWO filesystem purpose is supported')
    volume_name = pvc['spec']['volumeName']
    need(re.fullmatch(r'pvc-[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', volume_name) is not None,
         'bound volume identity malformed')
    pv = r['pv']
    pv_id = identity(pv, 'PersistentVolume', volume_name)
    ref = pv['spec']['claimRef']
    need((ref['namespace'], ref['name'], ref['uid']) == (*TARGET, claim['uid']), 'PV names a replacement claim')
    need(size(pv['spec']['capacity']['storage']) == before, 'PV capacity disagrees')
    csi = pv['spec']['csi']
    need(csi['driver'] == 'driver.longhorn.io' and csi['volumeHandle'] == volume_name and csi['fsType'] == 'ext4',
         'only the bound Longhorn ext4 device is supported')
    sc = r['sc']
    sc_id = identity(sc, 'StorageClass', 'longhorn-dev-disposable')
    need(pvc['spec']['storageClassName'] == sc_id['name'] and sc['provisioner'] == 'driver.longhorn.io'
         and sc['allowVolumeExpansion'] is True, 'expansion class unavailable')
    volume = r['volume']
    volume_id = identity(volume, 'Volume', volume_name, 'longhorn-system')
    vs, status = volume['spec'], volume['status']
    need(uint(vs['size']) == before, 'Longhorn size disagrees')
    need(vs['frontend'] == 'blockdev' and vs['dataEngine'] == 'v1' and vs['encrypted'] is False
         and vs['accessMode'] == 'rwo', 'unsupported online expansion configuration')
    need(status['state'] == 'attached' and status['robustness'] == 'healthy', 'volume is not healthy and attached')
    bound = status['kubernetesStatus']
    need((bound['namespace'], bound['pvcName'], bound['pvName']) == (*TARGET, volume_name), 'Longhorn binding disagrees')
    replicas = [v for v in whole(r['replicas'], 'Replica', 'longhorn-system') if v['spec']['volumeName'] == volume_name]
    need(len(replicas) == uint(vs['numberOfReplicas']) and replicas, 'actual replica roster disagrees')
    for replica in replicas:
        spec = replica['spec']
        need(spec['active'] is True and spec['healthyAt'] and not spec['failedAt']
             and spec['nodeID'] and spec['diskID'], 'replica is inactive, unhealthy or unscheduled')
    nodes = whole(r['nodes'], 'Node', 'longhorn-system')
    op = r['overprovisioning']
    minimum = r['minimal_available']
    identity(op, 'Setting', 'storage-over-provisioning-percentage', 'longhorn-system')
    identity(minimum, 'Setting', 'storage-minimal-available-percentage', 'longhorn-system')
    opct, mpct = uint(op['value']), uint(minimum['value'])
    need(mpct <= 100, 'invalid minimum percentage')
    ledger = {'volume': volume, 'replicas': replicas, 'nodes': nodes, 'opct': opct, 'mpct': mpct,
              'growth': wanted - before}
    checked = subprocess.run(['bash', str(ROOT / 'infra/lib/check-longhorn-expansion.sh')],
                             input=json.dumps(ledger), text=True, capture_output=True, timeout=10, check=False)
    need(checked.returncode == 0, 'Longhorn admission cannot answer or refuses: ' + checked.stderr)
    disks = json.loads(checked.stdout)
    pods = [p for p in whole(r['pods'], 'Pod', TARGET[0]) if any(
        v.get('persistentVolumeClaim', {}).get('claimName') == TARGET[1] for v in p['spec'].get('volumes', []))]
    need(len(pods) == 1, 'claim must have exactly one current consumer')
    pod = pods[0]
    pod_id = identity(pod, 'Pod', namespace=TARGET[0])
    need(pod['status']['phase'] == 'Running' and pod['spec']['nodeName'] == status['currentNodeID'],
         'current pod and attached node disagree')
    work = [v['name'] for v in pod['spec']['volumes'] if v.get('persistentVolumeClaim', {}).get('claimName') == TARGET[1]]
    dev = [c for c in pod['spec']['containers'] if c['name'] == 'dev']
    need(len(work) == 1 and len(dev) == 1 and any(
        m['name'] == work[0] and m['mountPath'] == '/work' and not m.get('readOnly') for m in dev[0]['volumeMounts']),
        'dev container mount binding missing')
    containers = pod['status']['containerStatuses']
    dev_status = [c for c in containers if c['name'] == 'dev']
    need(len(dev_status) == 1 and dev_status[0]['ready'] is True and dev_status[0]['state']['running']['startedAt']
         and dev_status[0]['containerID'], 'dev execution identity missing')
    pod_id.update(node=pod['spec']['nodeName'], container_id=dev_status[0]['containerID'],
                  restart_count=uint(dev_status[0]['restartCount']), started_at=dev_status[0]['state']['running']['startedAt'])
    patch = [{'op': 'test', 'path': '/metadata/uid', 'value': claim['uid']},
             {'op': 'test', 'path': '/spec/resources/requests/storage', 'value': pvc['spec']['resources']['requests']['storage']},
             {'op': 'test', 'path': '/spec/volumeName', 'value': volume_name},
             {'op': 'replace', 'path': '/spec/resources/requests/storage', 'value': requested}]
    need(r['rights'] == {'patch_claim': True, 'read_mount': True}, 'executor capability not proven')
    # Longhorn 1.11.3 documents attached RWO blockdev ext4 expansion.
    # Bind the actual running implementation/configuration, not merely the
    # StorageClass flag. Actual mounted expansion is a separate postcondition.
    # https://longhorn.io/docs/1.11.3/nodes-and-volumes/volumes/expansion/
    need(qualified_longhorn_image(status['currentImage'], 'engine'), 'unqualified engine version')
    plugins = [p for p in whole(r['csi_pods'], 'Pod', 'longhorn-system') if p['spec'].get('nodeName') == pod_id['node']
               and any(c['name'] == 'longhorn-csi-plugin' for c in p['spec']['containers'])]
    need(len(plugins) == 1, 'current CSI plugin identity unavailable or ambiguous')
    plugin = plugins[0]
    implementation = [c for c in plugin['spec']['containers'] if c['name'] == 'longhorn-csi-plugin']
    runtime = [c for c in plugin['status']['containerStatuses'] if c['name'] == 'longhorn-csi-plugin']
    need(len(implementation) == len(runtime) == 1 and plugin['status']['phase'] == 'Running'
         and qualified_longhorn_image(implementation[0]['image'], 'manager') and runtime[0]['ready'] is True
         and runtime[0]['state']['running']['startedAt'] and runtime[0]['containerID']
         and re.search(r'@sha256:[0-9a-f]{64}$', runtime[0]['imageID']), 'current CSI implementation unqualified')
    mount = r['mount']
    need(mount['filesystem'] == 'ext4' and mount['source'] == '/dev/longhorn/'+volume_name
         and mount['mountpoint'] == '/work' and re.fullmatch(r'[0-9]+:[0-9]+', mount['device'])
         and mount['boot_id'] and mount['processes'], 'actual mount/session continuity binding unavailable')
    processes = sorted(mount['processes'], key=lambda x: x['pid'])
    need(len({uint(p['pid']) for p in processes}) == len(processes)
         and all(uint(p['starttime']) > 0 and p['name'] for p in processes), 'process identities malformed')
    # The available-byte reading is retained in raw evidence and checked live;
    # ordinary record growth must not void an otherwise unchanged signed plan.
    uint(mount['available_bytes'])
    mount_capacity = uint(mount['capacity_bytes'])
    need(0 < mount_capacity <= before and mount['available_bytes'] <= mount_capacity,
         'actual mounted filesystem capacity unavailable or disagrees')
    need(mount['available_bytes'] + wanted - before > 18*GIB,
         'requested growth does not project mounted headroom above the unchanged18GiB floor')
    binding = {k: mount[k] for k in ('filesystem', 'source', 'mountpoint', 'device', 'boot_id')}
    binding['processes'] = processes
    proposal = {'purpose': 'dev-work-volume', 'claim': dict(claim, namespace=TARGET[0]), 'pv': pv_id,
            'summary': f'Grow boss-dev/boss-dev-work from {before//GIB}Gi to {wanted//GIB}Gi; '
                       f'+{(wanted-before)//GIB}Gi per existing replica, '
                       f'{len(replicas)*(wanted-before)//GIB}Gi aggregate reservation',
            'storage_class': sc_id, 'volume': volume_id, 'pod': pod_id, 'current_bytes': before,
            'target_bytes': wanted, 'incremental_replica_bytes': len(replicas) * (wanted-before),
            'replicas': sorted([dict(identity(v, 'Replica'), node=v['spec']['nodeID'], disk=v['spec']['diskID'])
                                for v in replicas], key=lambda x: x['name']),
            'settings': {'overprovisioning': opct, 'minimal_available': mpct}, 'disks': disks,
            'online_support': {'basis': 'Longhorn v1.11.3 documented RWO/ext4/blockdev capability; actual growth still unproven',
                               'engine': status['currentImage'], 'plugin': identity(plugin, 'Pod'),
                               'image_id': runtime[0]['imageID'], 'container_id': runtime[0]['containerID']},
            'mount_binding': binding,
            'mounted_capacity_before': mount_capacity,
            'remaining_proof': 'independent complete source/evidence hashes, active-log prefix continuity and existing-owner unreadable-fixture attestation',
            'write': {'namespace': TARGET[0], 'claim': TARGET[1], 'patch': patch}}
    # Cost belongs to an authoritative read port, not to storage arithmetic
    # or the request's argv. Bind its entire original evidence to these stable
    # resource/session facts, then include both in the passkey-signed bytes.
    resource_hash = digest(proposal)
    monetary = cost_evidence(r.get('monetary_cost_evidence'), resource_hash)
    return dict(proposal, resource_plan_sha256=resource_hash, monetary_cost=monetary,
                executable=monetary['state'] == 'known')


def cost_evidence(value, resource_hash):
    """Validate the trusted read port's bounded original cost observation."""
    if value is None:
        return {'state':'unknown', 'required_evidence':
                'authoritative current incremental monetary cost for this exact resource plan; no native cost adapter exists'}
    fields = {'state','authority','reference','observed_at','valid_until',
              'resource_plan_sha256','amount','currency','billing_period'}
    need(type(value) is dict and set(value) == fields and value['state'] == 'known',
         'cost evidence is malformed or unknown')
    need(len(render(value).encode()) <= 16384, 'cost evidence exceeds bounded original record')
    for key in ('authority','reference','billing_period'):
        need(type(value[key]) is str and 0 < len(value[key]) <= 2048
             and all(c.isprintable() for c in value[key]), 'cost '+key+' is missing or malformed')
    need(type(value['amount']) is str and re.fullmatch(r'(0|[1-9][0-9]{0,11})(\.[0-9]{1,6})?',value['amount']) is not None,
         'cost amount is missing or malformed')
    need(type(value['currency']) is str and re.fullmatch(r'[A-Z]{3}',value['currency']) is not None,
         'cost currency is missing or malformed')
    observed, until = value['observed_at'], value['valid_until']
    need(type(observed) is int and type(until) is int
         and 0 < observed <= time.time() < until <= observed+900,
         'cost evidence is stale, future-dated or has an unbounded validity window')
    need(value['resource_plan_sha256'] == resource_hash, 'cost evidence names a different resource plan')
    return dict(value)


def digest(value):
    return hashlib.sha256(render(value).encode()).hexdigest()


def render(value):
    return json.dumps(value, sort_keys=True, indent=2) + '\n'


def render_manifest(template, claim):
    """Preserve only this live claim's approved growth, never edit the pod template."""
    documents = template.split('\n---\n')
    matches = [index for index, text in enumerate(documents) if re.search(
        r'^kind: PersistentVolumeClaim\nmetadata:\n  name: boss-dev-work\n  namespace: boss-dev$', text, re.M)]
    need(len(matches) == 1, 'fixed dev work PVC declaration is unavailable or ambiguous')
    index = matches[0]
    need(len(re.findall(r'^      storage: 40Gi$', documents[index], re.M)) == 1,
         'dev work bootstrap declaration changed; reviewed renderer update required')
    if claim is None:
        return template
    identity(claim,'PersistentVolumeClaim',TARGET[1],TARGET[0])
    need(claim['spec']['storageClassName'] == 'longhorn-dev-disposable', 'live dev work class changed')
    current = claim['spec']['resources']['requests']['storage']
    need(40*GIB <= size(current) <= 64*GIB, 'live request outside approved dev work range; never shrink')
    documents[index] = re.sub(r'^      storage: 40Gi$', '      storage: '+current, documents[index], flags=re.M)
    return '\n---\n'.join(documents)


def execute(read, patch, requested, approved, wait_seconds=0):
    """Ports are explicit: one conditional write, bounded reads, no retry of growth."""
    need(re.fullmatch(r'[0-9a-f]{64}', approved) is not None, 'exact approved plan hash required')
    before = plan(read(), requested)
    need(before['executable'], 'cost is unknown; planning-only result cannot authorize a write')
    need(digest(before) == approved, 'approved plan drift; nothing patched')
    # Re-read every physical admission and stable binding immediately before CAS.
    current = plan(read(), requested)
    need(current['executable'], 'cost is unknown before write; nothing patched')
    need(digest(current) == approved, 'plan drift before write; nothing patched')
    patch(current['write']['patch'])
    deadline = time.monotonic() + wait_seconds
    while True:
        after = read()
        pvc, pv, volume = after['pvc'], after['pv'], after['volume']
        need(identity(pvc, 'PersistentVolumeClaim', TARGET[1], TARGET[0])['uid'] == before['claim']['uid'],
             'request stands; claim identity changed, effect unproven')
        need(identity(pv, 'PersistentVolume')['uid'] == before['pv']['uid']
             and identity(volume, 'Volume')['uid'] == before['volume']['uid'],
             'request stands; bound volume identity changed, effect unproven')
        need(pvc['spec']['volumeName'] == before['pv']['name']
             and all(pv['spec']['claimRef'][k] == v for k,v in {'namespace':TARGET[0],'name':TARGET[1],'uid':before['claim']['uid']}.items()),
             'request stands; claim/volume binding changed, effect unproven')
        pods = whole(after['pods'], 'Pod', TARGET[0])
        matches = [p for p in pods if p['metadata']['uid'] == before['pod']['uid']]
        need(len(matches) == 1, 'request stands; original pod missing, effect unproven')
        pod = matches[0]
        dev = [c for c in pod['status']['containerStatuses'] if c['name'] == 'dev']
        need(pod['status']['phase'] == 'Running' and pod['spec']['nodeName'] == before['pod']['node']
             and len(dev) == 1 and dev[0]['containerID'] == before['pod']['container_id']
             and dev[0]['restartCount'] == before['pod']['restart_count'] and dev[0]['ready'] is True
             and dev[0]['state']['running']['startedAt'] == before['pod']['started_at'],
             'request stands; live process container changed, effect unproven')
        mount = after['mount']
        baseline = before['mount_binding']
        need(all(mount[k] == baseline[k] for k in ('filesystem','source','mountpoint','device','boot_id'))
             and all(p in mount['processes'] for p in baseline['processes']),
             'request stands; mount/session continuity changed, effect unproven')
        wanted = before['target_bytes']
        ready = (size(pvc['spec']['resources']['requests']['storage']) == wanted
                 and size(pvc['status']['capacity']['storage']) == wanted
                 and size(pv['spec']['capacity']['storage']) == wanted and uint(volume['spec']['size']) == wanted
                 and pvc['status'].get('conditions', []) == [] and volume['status']['robustness'] == 'healthy'
                 and before['mounted_capacity_before'] < uint(mount['capacity_bytes']) <= wanted)
        if ready:
            available = uint(mount['available_bytes'])
            need(available > 18 * GIB, 'request stands; mounted free space does not exceed 18 GiB')
            return {'plan_sha256': approved, 'claim_uid': before['claim']['uid'], 'pod_uid': before['pod']['uid'],
                    'mounted_available_bytes': available, 'capacity_bytes': wanted,
                    'mounted_capacity_bytes': uint(mount['capacity_bytes']),
                    'record_conservation': 'unproven; independent evidence required'}
        need(time.monotonic() < deadline, 'request stands; capacity/filesystem expansion is not proven; no growth retry')
        time.sleep(min(10, max(0, deadline-time.monotonic())))


# Fixed read-only program; neither paths nor code come from request arguments.
MOUNT_READ = r'''
import json,os,pathlib
rows=[]
for line in pathlib.Path('/proc/self/mountinfo').read_text().splitlines():
    fields=line.split()
    if fields[4]=='/work':
        sep=fields.index('-')
        rows.append(dict(device=fields[2],mountpoint=fields[4],filesystem=fields[sep+1],source=fields[sep+2]))
if len(rows)!=1:raise RuntimeError('work mount identity unavailable')
processes=[]
for directory in pathlib.Path('/proc').iterdir():
    if not directory.name.isdigit():continue
    try:
        data=(directory/'stat').read_text()
        end=data.rfind(')')
        name=data[data.index('(')+1:end]
        if name.startswith(('codex','claude','tmux','sshd')):
            processes.append(dict(pid=int(directory.name),name=name,starttime=int(data[end+2:].split()[19])))
    except FileNotFoundError:continue
if not processes:raise RuntimeError('live session processes unavailable')
st=os.statvfs('/work')
rows[0].update(available_bytes=st.f_bavail*st.f_frsize,
              capacity_bytes=st.f_blocks*st.f_frsize,
              boot_id=pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip(),processes=processes)
print(json.dumps(rows[0],sort_keys=True))
'''


class Native:
    """Existing operator kubectl adapter. Every original stream and exit retained."""
    def __init__(self, kubectl, budget_seconds=180):
        need(0 < budget_seconds <= 800, 'native deadline budget invalid')
        self.kubectl = kubectl
        self.records = []
        self.deadline = time.monotonic() + budget_seconds

    def call(self, args, json_result=True):
        remaining = self.deadline-time.monotonic()
        need(remaining > 0, 'native read deadline exhausted; no later command started')
        try:
            output = subprocess.run(self.kubectl + args, capture_output=True, timeout=min(20,remaining), check=False)
            stdout, stderr, code = output.stdout, output.stderr, output.returncode
        except subprocess.TimeoutExpired as error:
            stdout, stderr, code = error.stdout or b'', error.stderr or b'', None
        self.records.append({'argv': args, 'exit': code, 'stdout_b64': base64.b64encode(stdout).decode(),
                             'stderr_b64': base64.b64encode(stderr).decode(),
                             'stdout_bytes': len(stdout), 'stdout_sha256': hashlib.sha256(stdout).hexdigest(),
                             'stderr_bytes': len(stderr), 'stderr_sha256': hashlib.sha256(stderr).hexdigest()})
        need(code == 0, 'native read/write failed or timed out; original streams retained')
        try:
            return decode_native(stdout) if json_result else stdout.decode().strip()
        except (ValueError, UnicodeError) as error:
            raise ValueError('native response is not one whole typed JSON document') from error

    def get(self, resource, name=None, namespace=None):
        args = ['get', resource]
        if name is not None:
            args.append(name)
        if namespace is not None:
            args += ['-n', namespace]
        return self.call(args + ['-o', 'json', '--request-timeout=20s'])

    def read(self):
        pvc = self.get('pvc', TARGET[1], TARGET[0])
        identity(pvc, 'PersistentVolumeClaim', TARGET[1], TARGET[0])
        volume = pvc['spec']['volumeName']
        need(re.fullmatch(r'pvc-[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', volume) is not None,
             'invalid bound volume name')
        pods = self.get('pods', namespace=TARGET[0])
        consumers = [p for p in whole(pods, 'Pod', TARGET[0]) if any(
            v.get('persistentVolumeClaim', {}).get('claimName') == TARGET[1] for v in p['spec'].get('volumes', []))]
        need(len(consumers) == 1, 'current work claim consumer unavailable or ambiguous')
        pod = consumers[0]['metadata']['name']
        need(re.fullmatch(r'[a-z0-9][a-z0-9.-]{0,252}', pod) is not None, 'pod name malformed')
        # No authoritative billing adapter has been approved or implemented.
        # Never infer zero from existing disks/replicas or accept a caller's
        # cost assertion. Native planning is honest; native writes stay shut.
        reads = {'monetary_cost_evidence': None, 'pvc': pvc, 'pv': self.get('pv', volume),
                 'sc': self.get('storageclass', 'longhorn-dev-disposable'),
                 'volume': self.get('volumes.longhorn.io', volume, 'longhorn-system'),
                 'replicas': self.get('replicas.longhorn.io', namespace='longhorn-system'),
                 'nodes': self.get('nodes.longhorn.io', namespace='longhorn-system'),
                 'overprovisioning': self.get('settings.longhorn.io', 'storage-over-provisioning-percentage', 'longhorn-system'),
                 'minimal_available': self.get('settings.longhorn.io', 'storage-minimal-available-percentage', 'longhorn-system'),
                 'pods': pods, 'csi_pods': self.get('pods', namespace='longhorn-system')}
        reads['rights'] = {
            'patch_claim': self.call(['auth','can-i','patch','pvc/boss-dev-work','-n','boss-dev'], False) == 'yes',
            'read_mount': self.call(['auth','can-i','create','pods','--subresource=exec','-n','boss-dev'], False) == 'yes'}
        reads['mount'] = self.call(['exec','-n','boss-dev',pod,'-c','dev','--request-timeout=20s','--','python3','-c',MOUNT_READ])
        # Exec addresses a pod by name, not UID. Reconcile both sides of
        # that read so a replacement cannot inherit the old pod's receipt.
        observed = consumers[0]
        back = self.get('pod',pod,TARGET[0])
        identity(back,'Pod',pod,TARGET[0])
        def execution(p):
            return (p['metadata']['uid'],p['spec']['nodeName'],
                    [(c['containerID'],c['restartCount'],c['state'].get('running'))
                     for c in p['status']['containerStatuses'] if c['name']=='dev'])
        need(execution(back) == execution(observed), 'pod identity changed during mount read')
        reads['pods']['items'] = [back if p['metadata']['uid'] == observed['metadata']['uid'] else p
                                 for p in reads['pods']['items']]
        claim_back = self.get('pvc',TARGET[1],TARGET[0])
        identity(claim_back,'PersistentVolumeClaim',TARGET[1],TARGET[0])
        need(all(claim_back['metadata'][k] == pvc['metadata'][k] for k in ('uid','name','namespace'))
             and claim_back['spec']['volumeName'] == pvc['spec']['volumeName']
             and claim_back['spec']['resources']['requests'] == pvc['spec']['resources']['requests'],
             'claim identity or request changed during native observation')
        reads['pvc'] = claim_back
        return reads

    def patch(self, value):
        # The paired planner's whole native evidence must fit its existing
        # complete-stream carrier before we can ask for an effect. Leave room
        # for the subsequent readback; a larger record is a refusal, not a tail.
        need(len(self.evidence_bytes()) <= 30000, 'native evidence exceeds pre-write capture budget; nothing patched')
        self.call(['patch','pvc','boss-dev-work','-n','boss-dev','--type=json','-p',json.dumps(value),
                   '-o','json','--request-timeout=20s'])

    def evidence_bytes(self):
        raw = render(self.records).encode()
        # Lossless compression, not a digest substituted for the originals.
        return (json.dumps({'evidence_encoding':'gzip+base64','complete':True,'original_bytes':len(raw),
                            'request_id':os.environ.get('OPS_REQUEST_ID'),
                            'original_sha256':hashlib.sha256(raw).hexdigest(),
                            'data':base64.b64encode(gzip.compress(raw,mtime=0)).decode()})+'\n').encode()

    def evidence(self, recorder=None, failure=None):
        output = self.evidence_bytes()
        if failure is not None:
            envelope=decode_native(output)
            envelope['failure']=failure
            output=(json.dumps(envelope)+'\n').encode()
        if len(output) <= 90000:
            sys.stderr.write(output.decode())
            return
        # 1200f7a7: the real complete pod roster produced a951361-byte
        # envelope. Printing before checking the cap erased its refusal.
        # Keep the runner's cap: use its existing verified file record door.
        spec=importlib.util.spec_from_file_location('dev_work_evidence',ROOT/'infra/forge/dev-work-evidence.py')
        carrier=importlib.util.module_from_spec(spec)
        spec.loader.exec_module(carrier)
        request=decode_native(output).get('request_id')
        reference=(carrier.record if recorder is None else recorder)(output,request,failure)
        encoded=(json.dumps(reference)+'\n').encode()
        need(len(encoded)<=90000,'evidence reference exceeds complete-stream budget')
        sys.stderr.write(encoded.decode())


def main(args):
    import shlex
    if len(args)==4 and args[0]=='--decode-evidence':
        # Files are downloaded by id through the existing /api/files reader.
        # This offline consumer has no credential, URL or command parameter.
        spec=importlib.util.spec_from_file_location('dev_work_evidence',ROOT/'infra/forge/dev-work-evidence.py')
        carrier=importlib.util.module_from_spec(spec);spec.loader.exec_module(carrier)
        with Path(args[1]).open('rb') as source:
            raw=source.read(carrier.MAX_PACKET+1)
        need(len(raw)<=carrier.MAX_PACKET,'ops evidence packet exceeds decoder byte bound')
        supplied=decode_native(raw)
        if type(supplied) is dict and supplied.get('kind')=='ops-request':
            reference,transport=carrier.from_packet(supplied,args[3])
        else:
            reference=supplied
            transport={'state':'reference-only; ops carrier not verified','plan_success':False}
        directory=Path(args[2])
        decoded=carrier.restore(reference,args[3],lambda file_id:carrier.read_part(directory,file_id),include_failure=True)
        if transport['plan_success']:
            need(decoded['failure'] is None and decoded['records']
                 and all(record['exit']==0 for record in decoded['records']),
                 'successful ops carrier contains failed native observations')
        decoded['carrier']=transport
        print(render(decoded),end='')
        return
    if len(args) == 3 and args[0] == '--render-converge':
        template=Path(args[1])
        raw=Path(args[2]).read_text()
        output=render_manifest(template.read_text(),decode_native(raw) if raw.strip() else None)
        template.write_text(output)
        return
    planning = len(args) == 2 and args[0] == '--plan'
    need(planning or len(args) == 2, 'usage: --plan <size> OR <size> <approved-plan-sha256>')
    requested = args[1] if planning else args[0]
    need(re.fullmatch(r'(4[1-9]|5[0-9]|6[0-4])Gi', requested) is not None, 'fixed41..64Gi purpose only')
    need(planning or re.fullmatch(r'[0-9a-f]{64}',args[1]) is not None, 'exact approved-plan hash required')
    # Parse the door from the trusted wrapper, never from a request parameter.
    door = shlex.split(os.environ.get('BOSS_DEV_WORK_KUBECTL', ''))
    need(door, 'operator kubectl door unavailable')
    native = Native(door, budget_seconds=180 if planning else 800)
    failure=None
    try:
        if planning:
            proposal = plan(native.read(), requested)
        else:
            receipt = execute(native.read,native.patch,requested,args[1],wait_seconds=600)
    except Exception as error:
        failure=error
    try:
        native.evidence(failure=None if failure is None else str(failure))
    except (ValueError,OSError,subprocess.SubprocessError) as error:
        prerequisite=str(failure)
        if len(prerequisite)>4000:
            prerequisite='refused; original diagnostic exceeds inline bound'
        raise ValueError('complete evidence unavailable; no proposal/effect; '
                         +'prerequisite: '+prerequisite+'; carrier: '+str(error)) from error
    if failure is not None:
        if len(str(failure))>4000:
            raise ValueError('prerequisite refused; complete original diagnostic is in native evidence') from failure
        raise failure
    if planning:
        print(render(proposal), end='')
        print('plan-sha256: '+digest(proposal),file=sys.stderr)
    else:
        print(render(receipt),end='')


if __name__ == '__main__':
    try:
        main(sys.argv[1:])
    except (ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as failure:
        print('dev-work-volume: CANNOT ANSWER / REFUSED — '+str(failure),file=sys.stderr)
        sys.exit(78)
