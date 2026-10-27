#!/usr/bin/env python3
"""Fixed database observations, not an attribution of a production stall."""
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import sys
import tempfile
import time

# Reuse the already bounded, lossless native transport. Its gate topology
# decoder is deliberately not used: this target is the database's CSI volume.
_spec = importlib.util.spec_from_file_location(
    'bounded_native_observation', Path(__file__).with_name('gate-storage.py'))
bounded = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bounded)
UUID = r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}'
DNS = r'[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?'
NATIVE_BYTE_BUDGET = 256 * 1024


def native_name(value):
    return len(value) <= 253 and re.fullmatch(DNS, value)


def row(raw, count):
    lines = raw.splitlines()
    if len(lines) != 1:
        raise ValueError('native identity row missing or ambiguous')
    fields = lines[0].split('\t')
    if len(fields) != count or any(not field for field in fields):
        raise ValueError('native identity field missing or ambiguous')
    return fields


def identity(rows):
    ns, pod, pod_uid, node, kind, owner, owner_uid, phase, container, restarts, mount, claim = row(rows['pod'], 12)
    sts_ns, sts, sts_uid = row(rows['statefulset'], 3)
    pvc_ns, pvc, pvc_uid, pvc_phase, volume = row(rows['pvc'], 5)
    pv, pv_uid, claim_ns, claim_name, claim_uid, driver, handle = row(rows['pv'], 7)
    longhorn, longhorn_uid, longhorn_pv = row(rows['volume'], 3)
    if (ns != 'boss' or pod != 'postgres-0' or kind != 'StatefulSet'
            or (sts_ns, sts, sts_uid) != (ns, 'postgres', owner_uid)
            or owner != sts or phase != 'Running' or mount != 'pgdata'
            or (pvc_ns, pvc, pvc_phase) != (ns, claim, 'Bound')
            or (claim_ns, claim_name, claim_uid) != (ns, pvc, pvc_uid)
            or pv != volume or driver != 'driver.longhorn.io'
            or (longhorn, longhorn_pv) != (handle, pv)):
        raise ValueError('database Pod, owner, claim or CSI volume disagree')
    if (any(not re.fullmatch(UUID, value) for value in (pod_uid, sts_uid, pvc_uid, pv_uid, longhorn_uid))
            or any(not native_name(value) for value in (node, pvc, pv, handle))
            or not re.fullmatch(r'containerd://[0-9a-f]{64}', container)
            or not re.fullmatch(r'[0-9]+', restarts)):
        raise ValueError('database native lifetime is unread')
    return {'namespace': ns, 'pod': pod, 'pod_uid': pod_uid, 'node': node,
            'statefulset_uid': sts_uid, 'container_id': container, 'restarts': int(restarts),
            'pvc': pvc, 'pvc_uid': pvc_uid, 'pv': pv, 'pv_uid': pv_uid,
            'volume_handle': handle, 'volume_uid': longhorn_uid}


def io_delta(before_identity, after_identity, before, after):
    if not isinstance(before_identity, dict) or not isinstance(after_identity, dict):
        raise ValueError('database native lifetime unread; no accounting delta available')
    if not isinstance(before, dict) or not isinstance(after, dict):
        raise ValueError('database IO samples unread; no accounting delta available')
    if before_identity != after_identity:
        raise ValueError('database native lifetime changed')
    for name in ('boot_id', 'cgroup_identity', 'mount_device', 'mount_source'):
        if not before.get(name) or before[name] != after.get(name):
            raise ValueError('database IO lifetime changed or unread: ' + name)
    if (not re.fullmatch(UUID, before['boot_id'])
            or not re.fullmatch(r'[0-9]+ [0-9]+', before['cgroup_identity'])
            or not re.fullmatch(r'[0-9]+:[0-9]+', before['mount_device'])
            or before['mount_source'] != '/dev/longhorn/' + before_identity['volume_handle']):
        raise ValueError('database mount is not the observed Longhorn volume')
    for raw in (before['io_stat'], after['io_stat']):
        devices = set()
        for line in raw.splitlines():
            fields = line.split()
            if not fields or not re.fullmatch(r'[0-9]+:[0-9]+', fields[0]) or fields[0] in devices:
                raise ValueError('database IO device missing or duplicated')
            devices.add(fields[0])
            counters = [field.split('=') for field in fields[1:]]
            if (any(len(field) != 2 or not re.fullmatch(r'[a-z_]+', field[0]) or not re.fullmatch(r'[0-9]+', field[1]) for field in counters)
                    or len({field[0] for field in counters}) != len(counters)
                    or not {'rbytes', 'wbytes', 'rios', 'wios'} <= {field[0] for field in counters}):
                raise ValueError('database IO counter missing or ambiguous')
    try:
        result = bounded.io_delta(before['io_stat'], after['io_stat'], before['mount_device'])
    except KeyError as error:
        raise ValueError('database IO mount device unread') from error
    result['meaning'] = 'cgroup accounting on the verified database mount device; not latency or physical-media health'
    return result


def declared_target(target):
    """Validate only the declared target; this is not native binding evidence."""
    if (not isinstance(target, dict) or target.get('host') not in
            ('postgres', 'postgres.boss', 'postgres.boss.svc', 'postgres.boss.svc.cluster.local')
            or type(target.get('port')) is not int or target['port'] != 5432
            or not isinstance(target.get('database'), str)
            or not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_.-]{0,62}', target['database'])):
        raise ValueError('actual pool target is unknown or outside the fixed postgres service')
    return target


def bind_target(target, service, endpoints, bound):
    """Bind the pool's declared target to one ready native database lifetime."""
    declared_target(target)
    if not all(isinstance(value, dict) for value in (service, endpoints, bound)):
        raise ValueError('native database binding objects unread')
    metadata = service.get('metadata', {})
    if not isinstance(metadata, dict) or not isinstance(service.get('spec', {}), dict):
        raise ValueError('database service shape unread')
    if (metadata.get('namespace'), metadata.get('name')) != ('boss', 'postgres') or not re.fullmatch(UUID, metadata.get('uid', '')):
        raise ValueError('database service identity unread')
    ports = service.get('spec', {}).get('ports', [])
    if not isinstance(ports, list) or len(ports) != 1 or not isinstance(ports[0], dict) or any(ports[0].get(key) != value for key, value in
                             (('port', 5432), ('targetPort', 5432), ('protocol', 'TCP'))):
        raise ValueError('database service port does not bind the fixed postgres port')
    if service.get('spec', {}).get('type', 'ClusterIP') == 'ExternalName':
        raise ValueError('external database service is unsupported')
    em = endpoints.get('metadata', {})
    subsets = endpoints.get('subsets', [])
    if (not isinstance(em, dict) or (em.get('namespace'), em.get('name')) != ('boss', 'postgres')
            or not isinstance(em.get('uid'), str) or not re.fullmatch(UUID, em['uid'])
            or not isinstance(subsets, list) or len(subsets) != 1 or not isinstance(subsets[0], dict)):
        raise ValueError('database endpoints missing or ambiguous')
    subset = subsets[0]
    addresses, endpoint_ports = subset.get('addresses', []), subset.get('ports', [])
    if (subset.get('notReadyAddresses') or not isinstance(addresses, list) or not isinstance(endpoint_ports, list)
            or len(addresses) != 1 or len(endpoint_ports) != 1
            or not isinstance(addresses[0], dict) or not isinstance(endpoint_ports[0], dict)
            or endpoint_ports[0].get('port') != 5432 or endpoint_ports[0].get('protocol') != 'TCP'):
        raise ValueError('database endpoint readiness or port differs')
    ref = addresses[0].get('targetRef', {})
    if not isinstance(ref, dict) or (ref.get('kind'), ref.get('namespace'), ref.get('name'), ref.get('uid')) != ('Pod', 'boss', 'postgres-0', bound['pod_uid']):
        raise ValueError('database endpoint does not name the observed postgres Pod UID')
    return {**target, 'service_uid': metadata['uid'], 'endpoints_uid': em['uid'], 'pod_uid': bound['pod_uid']}


def projection(paths):
    return 'jsonpath=' + '{"\\t"}'.join('{' + path + '}' for path in paths) + '{"\\n"}'


POD = projection(('.metadata.namespace', '.metadata.name', '.metadata.uid', '.spec.nodeName',
                  '.metadata.ownerReferences[?(@.controller==true)].kind',
                  '.metadata.ownerReferences[?(@.controller==true)].name',
                  '.metadata.ownerReferences[?(@.controller==true)].uid', '.status.phase',
                  '.status.containerStatuses[?(@.name=="postgres")].containerID',
                  '.status.containerStatuses[?(@.name=="postgres")].restartCount',
                  '.spec.containers[?(@.name=="postgres")].volumeMounts[?(@.mountPath=="/var/lib/postgresql/data")].name',
                  '.spec.volumes[?(@.name=="pgdata")].persistentVolumeClaim.claimName'))
STS = projection(('.metadata.namespace', '.metadata.name', '.metadata.uid'))
PVC = projection(('.metadata.namespace', '.metadata.name', '.metadata.uid', '.status.phase', '.spec.volumeName'))
PV = projection(('.metadata.name', '.metadata.uid', '.spec.claimRef.namespace', '.spec.claimRef.name',
                 '.spec.claimRef.uid', '.spec.csi.driver', '.spec.csi.volumeHandle'))
VOLUME = projection(('.metadata.name', '.metadata.uid', '.status.kubernetesStatus.pvName'))
SQL = Path(__file__).with_name('jobs-database-observation.sql').read_text()


def sampling_sql(serving_database_known):
    if serving_database_known:
        return SQL
    # One definition of the server statistics. The marked database-local
    # field is omitted entirely when the jobs API's target is unread.
    query = SQL
    for section in ('database-local', 'serving-activity'):
        before, marker, rest = query.partition('-- ' + section + ' begin')
        _, end, after = rest.partition('-- ' + section + ' end')
        if not marker or not end:
            raise ValueError('fixed server-only statistics query unread')
        query = before + after
    return query


IO_READ = '''set -eu
printf 'BOOT\\n'; cat /proc/sys/kernel/random/boot_id
printf 'CGROUP\\n'; stat -c '%d %i' /sys/fs/cgroup
printf 'MOUNT\\n'; awk '$5 == "/var/lib/postgresql/data" {print}' /proc/self/mountinfo
printf 'IO_STAT\\n'; cat /sys/fs/cgroup/io.stat
printf 'IO_PRESSURE\\n'; cat /sys/fs/cgroup/io.pressure
'''


def io_sample(raw):
    sections = {}
    current = None
    for line in raw.splitlines():
        if line in ('BOOT', 'CGROUP', 'MOUNT', 'IO_STAT', 'IO_PRESSURE'):
            if line in sections:
                raise ValueError('duplicate native IO section')
            current = line
            sections[current] = []
        elif current is None:
            raise ValueError('native IO section missing')
        else:
            sections[current].append(line)
    if set(sections) != {'BOOT', 'CGROUP', 'MOUNT', 'IO_STAT', 'IO_PRESSURE'}:
        raise ValueError('native IO section unread')
    if any(len(sections[key]) != 1 for key in ('BOOT', 'CGROUP', 'MOUNT')):
        raise ValueError('native IO identity missing or ambiguous')
    mount = sections['MOUNT'][0].split()
    if len(mount) < 10 or mount[4] != '/var/lib/postgresql/data' or mount.count('-') != 1:
        raise ValueError('database mountinfo unread')
    separator = mount.index('-')
    if separator + 2 >= len(mount):
        raise ValueError('database mount source unread')
    return {'boot_id': sections['BOOT'][0], 'cgroup_identity': sections['CGROUP'][0],
            'mount_device': mount[2], 'mount_source': mount[separator + 2],
            'io_stat': '\n'.join(sections['IO_STAT']), 'io_pressure': '\n'.join(sections['IO_PRESSURE'])}


def collect(recorder, kubectl, sleep=time.sleep):
    result = {'state': 'unavailable', 'claim': 'database-observation-only', 'samples': []}
    def read(label, argv):
        # One scalar can cross this limit; it is retained but never parsed.
        # This keeps the raw record plus its derivation within one native upload.
        if recorder.bytes >= NATIVE_BYTE_BUDGET:
            raise ValueError('database observation native byte bound reached')
        observed = recorder.call(label, argv)
        if recorder.bytes > NATIVE_BYTE_BUDGET:
            raise ValueError('database observation native byte bound reached')
        return bounded.text(observed)
    def call(label, args):
        return read(label, [*kubectl, *args, '--request-timeout=15s'])
    def identify(prefix):
        rows = {'pod': call(prefix + '.pod', ['get', 'pod', 'postgres-0', '-n', 'boss', '-o', POD]),
                'statefulset': call(prefix + '.statefulset', ['get', 'statefulset', 'postgres', '-n', 'boss', '-o', STS])}
        claim = row(rows['pod'], 12)[11]
        if not native_name(claim):
            raise ValueError('native database claim unread')
        rows['pvc'] = call(prefix + '.pvc', ['get', 'pvc', claim, '-n', 'boss', '-o', PVC])
        volume = row(rows['pvc'], 5)[4]
        if not native_name(volume):
            raise ValueError('native database PV unread')
        rows['pv'] = call(prefix + '.pv', ['get', 'pv', volume, '-o', PV])
        handle = row(rows['pv'], 7)[6]
        if not native_name(handle):
            raise ValueError('native database CSI handle unread')
        rows['volume'] = call(prefix + '.volume', ['get', 'volumes.longhorn.io', handle, '-n', 'longhorn-system', '-o', VOLUME])
        return identity(rows)
    errors = []
    def attempt(label, action):
        try:
            return action()
        except (ValueError, KeyError, TypeError, OSError) as error:
            errors.append(label + ': ' + type(error).__name__ + ': ' + str(error))
            return None
    def target(prefix):
        value = bounded.one_json(read(prefix + '.target', ['bash', str(Path(__file__).with_name('read-jobs-database.sh')), '--read-target']))
        if (not isinstance(value.get('build_commit'), str) or not re.fullmatch(r'[0-9a-f]{40}', value['build_commit'])
                or type(value.get('process_id')) is not int or value['process_id'] <= 0):
            raise ValueError('protected serving process/build identity unread')
        return value
    def binding(prefix, observed, bound):
        service = bounded.one_json(call(prefix + '.service', ['get', 'service', 'postgres', '-n', 'boss', '-o', 'json']))
        endpoints = bounded.one_json(call(prefix + '.endpoints', ['get', 'endpoints', 'postgres', '-n', 'boss', '-o', 'json']))
        return bind_target(observed['target'], service, endpoints, bound)
    try:
        # A stalled API or unready endpoint must not blind the independent
        # PostgreSQL and mount observations (d9b39685, review30f88223 F1).
        # Missing binding still refuses the complete observation below.
        result['process_before'] = attempt('before.target', lambda: target('before'))
        result['before'] = attempt('before.identity', lambda: identify('before'))
        result['target_binding_before'] = attempt('before.binding', lambda: binding('before', result['process_before'], result['before']))
        declared = attempt('serving.target', lambda: declared_target(result['process_before']['target']))
        database = declared['database'] if declared else 'postgres'
        result['serving_database'] = declared['database'] if declared else None
        result['sampling_scope'] = 'serving-database' if declared else 'server-wide-unbound'
        sql_text = sampling_sql(declared is not None)
        for index in range(2):
            if index:
                sleep(1)
            prefix = 'sample' + str(index)
            native = [*kubectl, '-n', 'boss', '--request-timeout=15s', 'exec', 'postgres-0', '-c', 'postgres', '--', 'timeout', '10']
            def sql_read():
                value = bounded.one_json(read(prefix + '.sql', [*native, 'psql', '-X', '-q', '-At', '-v', 'ON_ERROR_STOP=1', '-U', 'boss', '-d', database,
                      '-c', 'SET default_transaction_read_only=on', '-c', 'SET statement_timeout=5000', '-c', 'SET lock_timeout=1000', '-c', sql_text]))
                if value.get('database') != database or value.get('transaction_read_only') != 'on':
                    raise ValueError('database positive target/read-only control differs')
                return value
            sql = attempt(prefix + '.sql', sql_read)
            io = attempt(prefix + '.io', lambda: io_sample(read(prefix + '.io', [*native, 'sh', '-c', IO_READ])))
            result['samples'].append({'sql': sql, 'io': io, 'monotonic': recorder.clock()})
        result['after'] = attempt('after.identity', lambda: identify('after'))
        result['process_after'] = attempt('after.target', lambda: target('after'))
        if result['after'] and result['process_after']:
            result['target_binding_after'] = attempt('after.binding', lambda: binding('after', result['process_after'], result['after']))
            if (result.get('target_binding_after') is not None and result['target_binding_before'] is not None
                    and result['process_before'] is not None and (result['target_binding_after'] != result['target_binding_before'] or any(
                    result['process_before'][key] != result['process_after'][key] for key in ('build_commit', 'process_id', 'target')))):
                errors.append('serving process, pool target or service endpoint lifetime changed')
        accounting = attempt('io_accounting', lambda: io_delta(result['before'], result['after'], result['samples'][0]['io'], result['samples'][1]['io']))
        if accounting is not None:
            result['io_accounting'] = accounting
        result['sample_elapsed_seconds'] = result['samples'][1]['monotonic'] - result['samples'][0]['monotonic']
        bounded.observation_bounds(recorder.rows)
        if not errors:
            result['state'] = 'observed'
    except (ValueError, KeyError, TypeError, OSError) as error:
        errors.append(type(error).__name__ + ': ' + str(error))
    if errors:
        result['errors'] = errors
        result['error'] = '; '.join(errors)
    result['readings'] = recorder.rows
    return result


def main(recorder_factory=bounded.Recorder):
    if len(sys.argv) != 1:
        print('read-jobs-database: REFUSED — no arguments accepted', file=sys.stderr)
        return 78
    request = os.environ.get('OPS_REQUEST_ID', '')
    if not re.fullmatch(UUID, request):
        print('read-jobs-database: REFUSED — native ops request identity required', file=sys.stderr)
        return 78
    work = Path(tempfile.mkdtemp(prefix='boss-database-observation-' + request + '-'))
    os.chmod(work, 0o700)
    def retain(row):
        with (work / 'readings.jsonl').open('a') as stream:
            stream.write(json.dumps(row, sort_keys=True) + '\n')
    recorder = recorder_factory(sink=retain, limit=21)
    try:
        kubectl = shlex.split(os.environ['DATABASE_OBSERVATION_KUBECTL'])
        if not kubectl:
            raise ValueError('existing native door unread')
        result = collect(recorder, kubectl)
        result['request'] = request
        result['source'] = {path.name: bounded.byte_record(path.read_bytes())['sha256'] for path in
                            (Path(__file__), Path(bounded.__file__), Path(__file__).with_name('jobs-database-observation.sql'),
                             Path(__file__).with_name('read-jobs-database.sh'))}
        path = work / 'observation.json'
        path.write_text(json.dumps(result, sort_keys=True) + '\n')
        if path.stat().st_size > 2_096_128:
            raise ValueError('observation exceeds native attachment bound; original retained')
        receipt = recorder.call('attach-observation', ['boss', 'attach', request, str(path)])
        native_receipt = bounded.text(receipt)
        print(json.dumps({'claim': result['claim'], 'state': result['state'], 'error': result.get('error'),
                          'retained_original': str(work), 'attachment_receipt': native_receipt}, sort_keys=True))
        return 0 if result['state'] == 'observed' else 1
    except (ValueError, KeyError, TypeError, OSError) as error:
        print(json.dumps({'state': 'unavailable', 'error': str(error), 'retained_original': str(work)}, sort_keys=True))
        return 1


if __name__ == '__main__':
    sys.exit(main())
