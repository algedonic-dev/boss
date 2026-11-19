#!/usr/bin/env python3
"""Purge-only, not snapshot deletion: urgent 34b7906a.

Manager/engine v1.11.3: app/recurringjob/volume.go, manager/engine.go,
and engine pkg/sync/rpc/server.go. Purge removes marked/system layers;
it cannot promise how many physical bytes coalescing will release.
"""
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import re
import shlex
import shutil
import sys
import tempfile
import time
from urllib.parse import urlencode

_spec = importlib.util.spec_from_file_location('purge_transport', Path(__file__).with_name('gate-storage.py'))
bounded = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bounded)
LH = 'longhorn-system'
IMAGE = {'longhornio/longhorn-engine:v1.11.3', 'docker.io/longhornio/longhorn-engine:v1.11.3'}
PROXY = '/api/v1/namespaces/longhorn-system/services/http:longhorn-backend:9500/proxy/v1/volumes/'
VOLUME = r'pvc-' + bounded.UUID


class Unproven(ValueError):
    """The mutation was attempted; a refusal would falsely say it was not."""


def require(condition, why):
    if not condition:
        raise ValueError(why)


def one(row, kind, name, namespace):
    meta = row.get('metadata', {})
    api = 'v1' if kind in ('PersistentVolumeClaim', 'PersistentVolume', 'Pod') else 'batch/v1' if kind == 'Job' else 'longhorn.io/v1beta2'
    require(row.get('apiVersion') == api and row.get('kind') == kind and isinstance(name, str)
            and re.fullmatch(bounded.DNS, name) and meta.get('name') == name
            and meta.get('namespace', '') == namespace and re.fullmatch(bounded.UUID, meta.get('uid', ''))
            and not meta.get('deletionTimestamp'), 'wrong, deleting or unread ' + kind + ' identity')
    return row


def whole(value, kind, namespace):
    meta = value.get('metadata', {})
    require(value.get('kind') == kind + 'List' and isinstance(value.get('items'), list)
            and not meta.get('continue') and meta.get('remainingItemCount', 0) == 0,
            'incomplete ' + kind + ' inventory')
    rows = value['items']
    # Kubernetes typed lists carry TypeMeta on the list, not each item.
    # Longhorn's unstructured CRD items still carry and must match theirs.
    if kind in ('Pod', 'Job'):
        api = 'v1' if kind == 'Pod' else 'batch/v1'
        require(value.get('apiVersion') == api, 'wrong typed list API')
        rows = [{**r, 'kind': r.get('kind', kind), 'apiVersion': r.get('apiVersion', api)} for r in rows]
    require(len({r.get('metadata', {}).get('name') for r in rows}) == len(rows)
            and len({r.get('metadata', {}).get('uid') for r in rows}) == len(rows), 'duplicate ' + kind + ' inventory')
    for row in rows:
        one(row, kind, row.get('metadata', {}).get('name'), namespace)
    return rows


def identity(row):
    return {k: row['metadata'][k] for k in ('name', 'uid')}


def holds(spec, pvc):
    volumes = spec.get('volumes', [])
    require(isinstance(volumes, list), 'unread claim volumes')
    return any(v.get('persistentVolumeClaim', {}).get('claimName') == pvc for v in volumes)


def idle_status(status, during):
    # Nullable maps are the published CRD shape; omitted fields are unknown.
    for field, active in [('backupStatus', None), ('restoreStatus', 'isRestoring'),
                          ('rebuildStatus', 'isRebuilding'), ('cloneStatus', 'isCloning'),
                          ('purgeStatus', 'isPurging')]:
        require(field in status and (status[field] is None or isinstance(status[field], dict)), 'unread ' + field)
        for row in (status[field] or {}).values():
            require(isinstance(row, dict) and not row.get('error'), 'failed ' + field)
            if field == 'purgeStatus' and during:
                continue
            if active:
                require(row.get(active) is False and row.get('state') in ('complete', ''), 'active or unknown ' + field)
            else:
                require(row.get('state') == 'complete' and row.get('progress') == 100, 'active or unknown backupStatus')


def qualify(b, namespace, pvc, during=False):
    p = one(b['pvc'], 'PersistentVolumeClaim', pvc, namespace)
    require(p['status']['phase'] == 'Bound', 'claim is not Bound')
    pv = one(b['pv'], 'PersistentVolume', p['spec']['volumeName'], '')
    ref, csi = pv['spec']['claimRef'], pv['spec']['csi']
    require((ref.get('uid'), ref.get('namespace'), ref.get('name')) == (p['metadata']['uid'], namespace, pvc)
            and csi.get('driver') == 'driver.longhorn.io' and re.fullmatch(VOLUME, csi.get('volumeHandle', '')),
            'PV CSI/claim binding differs')
    v = one(b['volume'], 'Volume', csi['volumeHandle'], LH)
    s, st = v['spec'], v['status']
    require(s.get('image') in IMAGE and st.get('currentImage') == s['image'] and s.get('dataEngine') == 'v1',
            'per-volume V1 engine must be exactly qualified 1.11.3')
    require(st.get('state') == 'attached' and st.get('robustness') == 'healthy'
            and s.get('nodeID') and st.get('currentNodeID') == s['nodeID']
            and not s.get('migrationNodeID') and not st.get('currentMigrationNodeID')
            and s.get('Standby') is False and st.get('isStandby') is False
            and st.get('restoreRequired') is False and st.get('restoreInitiated') is False,
            'volume is not attached healthy without migration/restore/standby')
    binding = st.get('kubernetesStatus', {})
    require((binding.get('namespace'), binding.get('pvcName')) == (namespace, pvc), 'Longhorn claim binding differs')
    replicas = [r for r in b['replicas'] if r['spec'].get('volumeName') == v['metadata']['name']]
    require(type(s.get('numberOfReplicas')) is int and s['numberOfReplicas'] > 0
            and len(replicas) == s['numberOfReplicas'], 'declared replica set is incomplete')
    for r in replicas:
        one(r, 'Replica', r['metadata']['name'], LH)
        require(r['spec'].get('active') is True and r['spec'].get('healthyAt') and not r['spec'].get('failedAt')
                and r['spec'].get('nodeID') and r['spec'].get('diskID') and r['status'].get('currentState') == 'running',
                'replica is not healthy active running')
    engines = [e for e in b['engines'] if e['spec'].get('volumeName') == v['metadata']['name']]
    require(len(engines) == 1, 'ambiguous engine set')
    e = engines[0]; one(e, 'Engine', e['metadata']['name'], LH)
    es, et = e['spec'], e['status']
    require(es.get('image') == s['image'] and et.get('currentImage') == s['image'] and es.get('dataEngine') == 'v1',
            'per-engine image is not qualified 1.11.3')
    require(es.get('active') is True and et.get('currentState') == 'running' and es.get('nodeID') == s['nodeID']
            and not es.get('requestedBackupRestore') and not et.get('snapshotsError') and not et.get('isExpanding'),
            'engine not running without restore/expansion/snapshot error')
    addresses = es.get('replicaAddressMap')
    names = {r['metadata']['name'] for r in replicas}
    require(isinstance(addresses, dict) and set(addresses) == names
            and et.get('currentReplicaAddressMap') == addresses
            and et.get('replicaModeMap') == {n: 'RW' for n in names}
            and len(set(addresses.values())) == len(names) and all(isinstance(a, str) and a for a in addresses.values()),
            'engine replica modes/addresses do not prove the complete healthy set')
    idle_status(et, during)
    if during:
        # The write was already accepted. Completion observes exact target
        # identities and health; it does not repeat namespace/snapshot censuses.
        return p, pv, v, replicas, e, [], set()
    for job in b['jobs']:
        if holds(job['spec']['template']['spec'], pvc):
            status = job.get('status', {})
            require(not job['metadata'].get('deletionTimestamp') and status.get('active', 0) == 0
                    and any(c.get('type') in ('Complete', 'Failed') and c.get('status') == 'True'
                            for c in status.get('conditions', [])), 'unfinished claim-holding Job ' + job['metadata']['name'])
    app_pods = []
    for pod in b['pods']:
        if holds(pod['spec'], pvc) and pod.get('status', {}).get('phase') not in ('Succeeded', 'Failed'):
            require(not any(o.get('kind') == 'Job' for o in pod['metadata'].get('ownerReferences', [])),
                    'live Job-owned claim Pod ' + pod['metadata']['name'])
            require(pod.get('status', {}).get('phase') in ('Pending', 'Running', 'Unknown'), 'unread application Pod phase')
            app_pods.append(identity(pod))
    # Backup CRs can precede engine status. Unknown ownership is not absence.
    protected = set()
    for backup in b['backups']:
        status, spec = backup.get('status', {}), backup.get('spec', {})
        require(status.get('volumeName'), 'backup ownership unknown')
        if status['volumeName'] == v['metadata']['name']:
            require(status.get('state') == 'Completed' and status.get('progress') == 100 and not status.get('error')
                    and spec.get('snapshotName') and status.get('snapshotName') == spec['snapshotName'], 'active or unknown Backup')
            protected.add(spec['snapshotName'])
    return p, pv, v, replicas, e, sorted(app_pods, key=lambda x: x['name']), protected


def render(b, namespace, pvc):
    p, pv, v, replicas, e, apps, protected = qualify(b, namespace, pvc)
    rows = b['snapshots']
    require(isinstance(rows, list) and rows and len({r.get('name') for r in rows}) == len(rows), 'missing or duplicate snapshots')
    names = {r['name'] for r in rows}
    require('volume-head' in names, 'live head is not observed')
    eligible = []; retained = []
    for row in rows:
        name = row['name']
        require(isinstance(name, str) and (name == 'volume-head' or re.fullmatch(bounded.DNS, name)), 'invalid snapshot name')
        require(type(row.get('removed')) is bool and type(row.get('usercreated')) is bool
                and isinstance(row.get('parent'), str) and isinstance(row.get('children'), dict), 'unread snapshot flags/chain')
        require(row['parent'] == '' or row['parent'] in names, 'missing snapshot parent')
        require(set(row['children']) <= names and all(v is True for v in row['children'].values()), 'unread snapshot children')
        if row['parent']:
            require(name in next(r['children'] for r in rows if r['name'] == row['parent']), 'non-reciprocal snapshot parent')
        require(all(next(r['parent'] for r in rows if r['name'] == child) == name for child in row['children']),
                'non-reciprocal snapshot child')
        seen = {name}; parent = row['parent']
        while parent:
            require(parent not in seen, 'cyclic snapshot chain')
            seen.add(parent); parent = next(r['parent'] for r in rows if r['name'] == parent)
        if name == 'volume-head':
            require(not row['removed'] and not row['children'], 'head cannot be removed or have children')
            continue
        require(isinstance(row.get('created'), str) and row['created'], 'snapshot creation time unknown')
        item = {k: row[k] for k in ('name', 'parent', 'children', 'removed', 'usercreated', 'created')}
        if row['removed'] or not row['usercreated']:
            require(name not in protected, 'purge would touch backup-bound snapshot ' + name)
            eligible.append(item)
        else:
            retained.append(item)
    # CR ownership must agree with the live engine snapshot, including backup labels.
    for cr in b['snapshot_crs']:
        if cr['spec'].get('volume') == v['metadata']['name']:
            require(not cr['metadata'].get('deletionTimestamp') and not cr.get('status', {}).get('error'), 'snapshot CR pending deletion/error')
            require(cr['metadata']['name'] in names, 'snapshot CR missing from live engine')
    require(eligible, 'no eligible system or removed snapshots; no action proposed')
    return {'schema': 1, 'operation': 'snapshotPurge', 'claim': {**identity(p), 'namespace': namespace},
            'pv': identity(pv), 'volume': identity(v), 'engine': {**identity(e), 'image': e['status']['currentImage']},
            'replicas': sorted([{**identity(r), 'node': r['spec']['nodeID'], 'disk': r['spec']['diskID'],
                                 'address': e['spec']['replicaAddressMap'][r['metadata']['name']]} for r in replicas], key=lambda x: x['name']),
            'eligible': sorted(eligible, key=lambda x: x['name']), 'retained_user_snapshots': sorted(retained, key=lambda x: x['name']),
            'protected_backup_snapshots': sorted(protected), 'application_pods': apps,
            'scope': 'volume-wide purge of all system and already removed layers; no snapshot/head/backup deletion or retention change',
            'limits': 'fresh reads are not a cross-resource lock; bytes reclaimed and disk-floor recovery are not promised'}


def digest(value):
    # Exactly the plan stdout that the ordinary approval runner hashes.
    return hashlib.sha256((json.dumps(value, sort_keys=True, indent=2) + '\n').encode()).hexdigest()


def publish(work, request, outcome, execute=bounded.native_command):
    """Originals must reach the record before an effect is printed.

    The native attachment door checks upload/download equality. Chunking
    conserves whole raw responses without exceeding its per-file bound.
    """
    require(re.fullmatch(bounded.UUID, request), 'ops request identity required for attachment')
    rows = [json.loads(p.read_text()) for p in work.glob('native-*.json')]
    require(all(type(row.get('sequence')) is int for row in rows), 'native receipt sequence is unknown; originals retained')
    rows.sort(key=lambda row: row['sequence'])
    require([row['sequence'] for row in rows] == list(range(1, len(rows) + 1)), 'native receipt sequence has gaps or duplicates; originals retained')
    previous = None
    for row in rows:
        begin, end = row.get('begin_monotonic'), row.get('end_monotonic')
        require(all(type(value) in (int, float) and math.isfinite(value) for value in (begin, end))
                and end >= begin and (previous is None or begin >= previous), 'native receipt time/order is unknown; originals retained')
        previous = end
    raw = json.dumps({'outcome': outcome, 'native': rows},
                     sort_keys=True, indent=2).encode() + b'\n'
    manifest = {'schema': 1, 'bytes': len(raw), 'sha256': hashlib.sha256(raw).hexdigest(), 'parts': []}
    deadline = time.monotonic() + 120
    def attach(path):
        require(time.monotonic() < deadline, 'native attachment deadline reached; originals retained')
        code, out, err, complete = execute(['boss', 'attach', request, str(path)], min(20, deadline - time.monotonic()))
        bounded.write_json(path.with_suffix('.receipt.json'), {'exit': code, 'complete': complete,
                           'stdout': bounded.byte_record(out), 'stderr': bounded.byte_record(err)})
        require(code == 0 and complete and b'byte-identical' in out, 'native attachment not verified; originals retained')
        return out.decode('utf-8', errors='strict')
    for offset in range(0, len(raw), 1024 * 1024):
        block = raw[offset:offset + 1024 * 1024]
        path = work / ('purge-evidence-' + str(offset) + '.json.bytes'); path.write_bytes(block)
        manifest['parts'].append({'name': path.name, 'bytes': len(block), 'sha256': hashlib.sha256(block).hexdigest(), 'receipt': attach(path)})
    path = work / 'purge-evidence-manifest.json'; bounded.write_json(path, manifest)
    receipt = attach(path)
    print('evidence-manifest: ' + receipt.strip(), file=sys.stderr)
    return manifest


def execute(read, action, namespace, pvc, approved, **options):
    attempted = False
    def fixed_action(volume):
        nonlocal attempted
        attempted = True
        return action(volume)
    try:
        return run_purge(read, fixed_action, namespace, pvc, approved, **options)
    except (ValueError, KeyError, TypeError, AttributeError, OSError, StopIteration) as error:
        if attempted:
            raise Unproven('purge requested; effect NOT proven: ' + str(error)) from error
        raise


def run_purge(read, action, namespace, pvc, approved, wait_seconds=600, sleep=time.sleep, clock=time.monotonic, poll=None):
    initial = read(); plan = render(initial, namespace, pvc)
    require(digest(plan) == approved, 'approved plan differs; nothing was purged')
    # A second independent inventory immediately precedes the fixed action.
    require(render(read(), namespace, pvc) == plan, 'plan moved before purge; nothing was purged')
    action(plan['volume']['name'])
    end = clock() + wait_seconds; witnessed = set()
    # V1 sync.Task reports replica URLs; the CR spec maps replica names to
    # bare addresses (engine_controller.go/GetBackendReplicaURL).
    expected = {'tcp://' + r['address'] for r in plan['replicas']}
    while True:
        after = poll(plan) if poll is not None else read()
        p, pv, v, replicas, engine, _, _ = qualify(after, namespace, pvc, during=True)
        require(identity(p) == {k: plan['claim'][k] for k in ('name', 'uid')}
                and identity(pv) == plan['pv'] and identity(v) == plan['volume']
                and identity(engine) == {k: plan['engine'][k] for k in ('name', 'uid')}
                and sorted([{**identity(r), 'node': r['spec']['nodeID'], 'disk': r['spec']['diskID'],
                             'address': engine['spec']['replicaAddressMap'][r['metadata']['name']]} for r in replicas], key=lambda x: x['name']) == plan['replicas'],
                'binding/engine/replica identity moved after purge; effect unknown')
        statuses = engine['status'].get('purgeStatus')
        require(isinstance(statuses, dict) and set(statuses) == expected, 'missing expected per-replica purge status')
        for address, status in statuses.items():
            require(isinstance(status, dict) and not status.get('error'), 'replica purge failed: ' + str(status))
            if status.get('isPurging') is True and status.get('state') == 'in_progress':
                witnessed.add(address)
        complete = all(s.get('isPurging') is False and s.get('state') == 'complete'
                       and type(s.get('progress')) is int and s['progress'] == 100 for s in statuses.values())
        if complete and witnessed == expected:
            return {'state': 'purge-complete', 'plan_sha256': approved, 'volume': plan['volume'],
                    'per_replica': statuses, 'capacity_recovery_proven': False,
                    'attribution': 'volume completion observed after request; a concurrent Longhorn purge cannot be distinguished',
                    'next': 'read fresh per-disk available bytes and the complete estate comparison separately'}
        require(clock() < end, 'fresh purge completion not proven for every replica; accepted request stands')
        sleep(10)


class NativeResponseTooLarge(ValueError):
    """A retained byte-capped response cannot be treated as a complete page."""


class Native:
    def __init__(self, argv, work, seconds=840):
        self.argv, self.work, self.index, self.total, self.end = argv, work, 0, 0, time.monotonic() + seconds
        self.mutation_attempted = False

    def call(self, suffix):
        require(self.index < 512 and self.total < 8 * 1024 * 1024 and time.monotonic() < self.end, 'operation read/time/byte bound reached')
        self.index += 1
        path = self.work / ('native-' + str(self.index) + '.json')
        record = {'sequence': self.index, 'begin_monotonic': time.monotonic(),
                  'argv': self.argv + suffix, 'state': 'started'}
        bounded.write_json(path, record)
        try:
            code, out, err, complete = bounded.native_command(self.argv + suffix, min(20, self.end - time.monotonic()))
            self.total += len(out) + len(err)
            record.update(state='finished', exit=code, stream_complete=complete,
                          stdout=bounded.byte_record(out), stderr=bounded.byte_record(err))
        except OSError as error:
            record.update(state='unavailable', error_type=type(error).__name__, errno=error.errno)
            raise
        finally:
            record['end_monotonic'] = time.monotonic()
            bounded.write_json(path, record)
        require(self.total <= 8 * 1024 * 1024, 'operation total byte bound reached')
        if not complete and len(out) + len(err) == bounded.SCALAR_BYTES:
            raise NativeResponseTooLarge('native response reached the per-call byte bound')
        require(code == 0 and complete, 'native command failed or incomplete: ' + err.decode('utf-8', errors='replace'))
        return bounded.one_json(out.decode('utf-8', errors='strict'))

    def get(self, resource, name, namespace):
        return self.call(['get', resource, name] + (['-n', namespace] if namespace else []) + ['-o', 'json', '--request-timeout=20s'])

    def listing(self, resource, kind, namespace):
        roster = {'replicas.longhorn.io': ('Replica', 'longhorn.io/v1beta2', 'replicas'),
                  'engines.longhorn.io': ('Engine', 'longhorn.io/v1beta2', 'engines'),
                  'snapshots.longhorn.io': ('Snapshot', 'longhorn.io/v1beta2', 'snapshots'),
                  'backups.longhorn.io': ('Backup', 'longhorn.io/v1beta2', 'backups'),
                  'jobs': ('Job', 'batch/v1', 'jobs'), 'pods': ('Pod', 'v1', 'pods')}
        require(resource in roster and roster[resource][0] == kind and re.fullmatch(bounded.DNS, namespace), 'outside fixed list roster')
        _, api, plural = roster[resource]
        base = '/api/v1' if api == 'v1' else '/apis/' + api
        base += '/namespaces/' + namespace + '/' + plural
        rows, tokens, token, version = [], set(), '', None
        page_size = 4 if kind in ('Pod', 'Job') else 32
        # A cursor belongs to its original query. A byte-cap refusal restarts
        # the entire collection; earlier rows and resourceVersion are discarded,
        # but every native attempt remains charged to the shared hard bounds.
        for _ in range(64):
            query = {'limit': page_size}
            if token:
                query['continue'] = token
            try:
                page = self.call(['get', '--raw', base + '?' + urlencode(query), '--request-timeout=20s'])
            except NativeResponseTooLarge:
                require(page_size > 1, 'one native list object exceeds the per-call byte bound; inventory incomplete')
                page_size = max(1, page_size // 2)
                rows, tokens, token, version = [], set(), '', None
                continue
            require(page.get('apiVersion') == api and page.get('kind') == kind + 'List'
                    and isinstance(page.get('items'), list), 'unread typed list page')
            meta = page.get('metadata', {}); rv = meta.get('resourceVersion')
            require(isinstance(rv, str) and rv and (version is None or version == rv), 'native list resource version changed or missing')
            version = rv; rows.extend(page['items'])
            token = meta.get('continue', '')
            require(isinstance(token, str) and token not in tokens, 'repeated or unknown list continuation')
            remaining = meta.get('remainingItemCount', 0)
            require(type(remaining) is int and remaining >= 0, 'unread remaining row count')
            if not token:
                require(remaining == 0, 'rows remain without continuation')
                return whole({'apiVersion': api, 'kind': kind + 'List', 'metadata': {}, 'items': rows}, kind, namespace)
            tokens.add(token)
        raise ValueError('native list page bound reached; inventory incomplete')

    def action(self, volume, action):
        require(re.fullmatch(VOLUME, volume) and action in ('snapshotList', 'snapshotPurge'), 'outside fixed action roster')
        if action == 'snapshotPurge':
            self.mutation_attempted = True
        answer = self.call(['create', '--raw', PROXY + volume + '?' + urlencode({'action': action}), '-f', '/dev/null', '--request-timeout=20s'])
        if action == 'snapshotPurge':
            require(answer.get('type') == 'volume' and answer.get('name') == volume, 'purge acknowledgement is not the named volume')
        return answer

    def read(self, namespace, pvc):
        p = self.get('pvc', pvc, namespace)
        pv_name = p.get('spec', {}).get('volumeName', '')
        require(re.fullmatch(bounded.DNS, pv_name), 'unread PV name')
        pv = self.get('pv', pv_name, '')
        volume = pv.get('spec', {}).get('csi', {}).get('volumeHandle', '')
        require(re.fullmatch(VOLUME, volume), 'unread Longhorn volume handle')
        result = {'pvc': p, 'pv': pv, 'volume': self.get('volumes.longhorn.io', volume, LH)}
        for key, resource, kind, ns in [('replicas', 'replicas.longhorn.io', 'Replica', LH), ('engines', 'engines.longhorn.io', 'Engine', LH),
                                       ('snapshot_crs', 'snapshots.longhorn.io', 'Snapshot', LH), ('backups', 'backups.longhorn.io', 'Backup', LH),
                                       ('jobs', 'jobs', 'Job', namespace), ('pods', 'pods', 'Pod', namespace)]:
            result[key] = self.listing(resource, kind, ns)
        live = self.action(volume, 'snapshotList')
        require(live.get('type') == 'collection' and isinstance(live.get('data'), list)
                and not live.get('pagination', {}).get('next'), 'incomplete live snapshot collection')
        result['snapshots'] = live['data']
        return result

    def poll(self, plan):
        # Exact named objects keep the original binding/health checks without
        # re-reading unrelated snapshot caches, backups or namespace workloads.
        return {'pvc': self.get('pvc', plan['claim']['name'], plan['claim']['namespace']),
                'pv': self.get('pv', plan['pv']['name'], ''),
                'volume': self.get('volumes.longhorn.io', plan['volume']['name'], LH),
                'engines': [self.get('engines.longhorn.io', plan['engine']['name'], LH)],
                'replicas': [self.get('replicas.longhorn.io', row['name'], LH) for row in plan['replicas']]}


def main():
    args = sys.argv[1:]; planning = bool(args and args[0] == '--plan')
    if planning:
        args = args[1:]
    require(len(args) == (2 if planning else 3), 'usage: --plan <instance> <claim> OR <instance> <claim> <approved-sha256>')
    namespace, pvc = args[:2]
    require(all(re.fullmatch(bounded.DNS, x) for x in (namespace, pvc)), 'invalid namespace or claim')
    require(namespace in os.environ.get('INSTANCE_PURGE_NAMESPACES', '').split(), 'namespace is not a declared instance')
    if not planning:
        require(re.fullmatch(r'[a-f0-9]{64}', args[2]), 'approved hash is not sha256')
    kubectl = shlex.split(os.environ.get('INSTANCE_PURGE_KUBECTL', ''))
    require(kubectl, 'trusted kubectl door unavailable')
    request = os.environ.get('OPS_REQUEST_ID', '')
    require(re.fullmatch(bounded.UUID, request), 'ops request identity is required before any native read')
    # Failed publication leaves these originals for rescue; success removes
    # them only after every part and the manifest have verified native ACKs.
    work = Path(tempfile.mkdtemp(prefix='instance-purge-'))
    print('native-originals: ' + str(work), file=sys.stderr)
    native = Native(kubectl, work, seconds=180 if planning else 840)
    outcome = {'state': 'unfinished'}
    try:
        if planning:
            plan = render(native.read(namespace, pvc), namespace, pvc)
            outcome = {'state': 'planned', 'plan_sha256': digest(plan), 'plan': plan}
        else:
            outcome = execute(lambda: native.read(namespace, pvc), lambda vol: native.action(vol, 'snapshotPurge'), namespace, pvc, args[2], poll=native.poll)
    except (ValueError, KeyError, TypeError, AttributeError, OSError) as error:
        outcome = {'state': 'refused-or-unproven', 'reason': str(error)}
        raise
    finally:
        try:
            publish(work, request, outcome)
        except (ValueError, OSError, KeyError, TypeError, AttributeError) as error:
            if native.mutation_attempted:
                raise Unproven('purge requested; evidence publication NOT proven: ' + str(error)) from error
            raise
        shutil.rmtree(work)
    if planning:
        print(json.dumps(plan, sort_keys=True, indent=2))
        print('plan-sha256: ' + digest(plan), file=sys.stderr)
    else:
        print(json.dumps(outcome, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except Unproven as error:
        print('instance-snapshot-purge: FAILED — ' + str(error), file=sys.stderr)
        sys.exit(1)
    except (ValueError, KeyError, TypeError, AttributeError, OSError, StopIteration) as error:
        print('instance-snapshot-purge: REFUSED or CANNOT ANSWER — ' + str(error), file=sys.stderr)
        sys.exit(78)
