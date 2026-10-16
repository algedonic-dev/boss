#!/usr/bin/env python3
"""Bounded native metadata, never a storage repair (design a3053877).

Talos API shape is pinned to the estate's v1.13.8 client, not latest:
https://github.com/siderolabs/talos/blob/v1.13.8/cmd/talosctl/cmd/talos/read.go
The CLI's error words cannot establish a typed NotFound or a healthy device.
"""
import base64
import datetime
import hashlib
import ipaddress
import json
import math
import os
from pathlib import Path
import re
import selectors
import shlex
import signal
import subprocess
import sys
import tempfile
import time


SCHEMA = 1
CALL_SECONDS = 20
TOTAL_SECONDS = 180
SCALAR_BYTES = 128 * 1024
TOTAL_BYTES = 8 * 1024 * 1024
MAX_READS = 128
CGROUP_FIELDS = ('io.stat', 'io.max', 'io.pressure', 'memory.stat', 'cpu.stat')
HOST_PATHS = ('/proc/diskstats', '/proc/meminfo', '/proc/pressure/io',
              '/proc/pressure/cpu', '/proc/pressure/memory')
LIFETIME_PATHS = ('/proc/sys/kernel/random/boot_id', '/proc/uptime', '/proc/version')
UUID = r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'
DNS = r'[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?'


def unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate JSON key')
        result[key] = value
    return result


def json_stream(text):
    decoder = json.JSONDecoder(object_pairs_hook=unique,
                               parse_constant=lambda _: (_ for _ in ()).throw(ValueError('nonfinite JSON')))
    rows = []
    while text.strip():
        row, end = decoder.raw_decode(text.lstrip())
        if not isinstance(row, dict):
            raise ValueError('native resource is not an object')
        rows.append(row)
        text = text.lstrip()[end:]
    if not rows:
        raise ValueError('empty native response')
    return rows


def one_json(text):
    rows = json_stream(text)
    if len(rows) != 1:
        raise ValueError('expected one native object')
    return rows[0]


def topology(pvc, pv):
    cm, pm, spec = pvc['metadata'], pv['metadata'], pv['spec']
    if (cm['name'], cm['namespace'], pvc['status']['phase']) != ('gate', 'boss-dev', 'Bound'):
        raise ValueError('gate PVC is not bound')
    ref = spec['claimRef']
    if (ref['uid'], ref['namespace'], ref['name']) != (cm['uid'], 'boss-dev', 'gate'):
        raise ValueError('PV claim identity differs')
    if not cm['uid'] or not pm['uid'] or pm['name'] != pvc['spec']['volumeName']:
        raise ValueError('PV identity differs')
    if spec['local']['path'] != '/var/mnt/gate':
        raise ValueError('PV does not name the gate user volume')
    terms = spec['nodeAffinity']['required']['nodeSelectorTerms']
    if len(terms) != 1 or set(terms[0]) != {'matchExpressions'}:
        raise ValueError('ambiguous gate PV affinity')
    expressions = terms[0]['matchExpressions']
    if len(expressions) != 1:
        raise ValueError('ambiguous gate node')
    expression = expressions[0]
    values = expression['values']
    if expression['key'] != 'kubernetes.io/hostname' or expression['operator'] != 'In' or len(values) != 1 or not re.fullmatch(DNS, values[0]):
        raise ValueError('unread gate node selector')
    return {'node': values[0], 'pvc_uid': cm['uid'], 'pv_uid': pm['uid'], 'pv': pm['name']}


def device(volumes, disks, address):
    if len(volumes) != 1:
        raise ValueError('ambiguous gate volume')
    volume = volumes[0]
    if volume['node'] != address or volume['metadata']['id'] != 'u-gate' or volume['metadata'].get('type') != 'VolumeStatuses.block.talos.dev':
        raise ValueError('volume response has wrong identity')
    spec = volume['spec']
    if spec.get('phase') != 'ready' or spec.get('filesystem') != 'xfs' or spec.get('mountSpec', {}).get('targetPath') != 'gate' or spec.get('mountSpec', {}).get('parentId') != '/var/mnt':
        raise ValueError('gate volume is not ready at its declared mount')
    if not spec['parentLocation'].startswith('/dev/') or not spec['location'].startswith('/dev/'):
        raise ValueError('block identity is not an absolute device path')
    whole = spec['parentLocation'].removeprefix('/dev/')
    partition = spec['location'].removeprefix('/dev/')
    if not re.fullmatch(r'nvme[0-9]+n[0-9]+', whole) or not re.fullmatch(re.escape(whole) + r'p[0-9]+', partition) or not spec['partitionUUID']:
        raise ValueError('unread gate block identity')
    matches = [d for d in disks if d.get('metadata', {}).get('id') == whole]
    if len(matches) != 1 or matches[0].get('node') != address or matches[0]['metadata'].get('type') != 'Disks.block.talos.dev':
        raise ValueError('disk response has wrong identity')
    disk = matches[0]['spec']
    if disk['dev_path'] != '/dev/' + whole or not disk.get('wwid') or not disk.get('serial') or not disk.get('model'):
        raise ValueError('stable native disk identity unavailable')
    pci = re.search(r'/([0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7])/nvme$', disk.get('bus_path', ''))
    return {'whole': whole, 'partition': partition, 'controller': whole.rsplit('n', 1)[0],
            'partition_uuid': spec['partitionUUID'], 'wwid': disk['wwid'], 'serial': disk['serial'], 'model': disk['model'],
            'pci': pci[1] if pci else None}


def worker(shape, registry):
    node, address = registry['node'], registry['address']
    ipaddress.IPv4Address(address)
    if registry['role'] != 'talos-worker' or node['metadata']['name'] != shape['node'] or not node['metadata']['uid']:
        raise ValueError('registry worker and Node differ')
    if any(key in node['metadata'].get('labels', {}) for key in ('node-role.kubernetes.io/control-plane', 'node-role.kubernetes.io/master')):
        raise ValueError('control plane is not the gate worker')
    if [a['address'] for a in node['status']['addresses'] if a['type'] == 'InternalIP'] != [address]:
        raise ValueError('native Node address differs')
    return address, node['metadata']['uid']


def client_path(argv):
    """The trusted ops door mounts exactly this public executable read-only."""
    mounts = [arg for arg in argv if arg.endswith(',dst=/talosctl,readonly')]
    if len(mounts) != 1 or not mounts[0].startswith('type=bind,src=/'):
        raise ValueError('actual selected Talos client path unavailable')
    path = mounts[0][len('type=bind,src='):-len(',dst=/talosctl,readonly')]
    if ',' in path or '\n' in path or '..' in Path(path).parts or not path:
        raise ValueError('unread selected Talos client path')
    return path


def bound_text(row, prefix, suffix):
    if row.get('argv') != prefix + suffix:
        raise ValueError('native operation/target differs: ' + row['operation'])
    return text(row)


def client_hash(row, selected_client):
    value = bound_text(row, ['sha256sum'], ['--', selected_client])
    match = re.fullmatch(r'([a-f0-9]{64})  ' + re.escape(selected_client) + r'\n', value)
    if not match:
        raise ValueError('actual selected Talos client hash unavailable')
    return match[1]


def replay_identity(rows, prefix, kube, talos, registry):
    pvc = one_json(bound_text(rows[prefix + '.pvc'], kube, ['get', 'pvc', 'gate', '-n', 'boss-dev', '-o', 'json', '--request-timeout=20s']))
    pv_name = pvc['spec']['volumeName']
    if not re.fullmatch(DNS, pv_name):
        raise ValueError('PV name outside native DNS shape')
    pv = one_json(bound_text(rows[prefix + '.pv'], kube, ['get', 'pv', pv_name, '-o', 'json', '--request-timeout=20s']))
    shape = topology(pvc, pv)
    native_registry = one_json(bound_text(rows[prefix + '.registry'], ['bash', registry], [shape['node']]))
    address, uid = worker(shape, native_registry)
    volumes = json_stream(bound_text(rows[prefix + '.volume'], talos, ['-n', address, 'get', 'volumestatus', 'u-gate', '-o', 'json']))
    disks = json_stream(bound_text(rows[prefix + '.disks'], talos, ['-n', address, 'get', 'disks', '-o', 'json']))
    return {**shape, 'node_uid': uid, 'address': address, 'device': device(volumes, disks, address)}


def replay_cohort(rows, prefix, kube, identity):
    node = identity['node']
    pods = bound_text(rows[prefix + '.pods'], kube, ['get', 'pods', '-n', 'boss-dev', '-l', 'app=gate-runner', '--field-selector', 'spec.nodeName=' + node, '-o', PODS, '--request-timeout=20s'])
    jobs = bound_text(rows[prefix + '.jobs'], kube, ['get', 'jobs', '-n', 'boss-dev', '-l', 'app=gate-runner', '-o', JOBS, '--request-timeout=20s'])
    return cohort(pods, jobs, node)


def sensor_directory(raw):
    lines = raw.splitlines()
    headers = [line for line in lines if line.split() == ['NODE', 'NAME']]
    if len(headers) != 1:
        raise ValueError('unread pinned Talos list shape')
    names = [line for line in lines if line not in headers and line != '.']
    if len(names) != 1 or not re.fullmatch(r'hwmon[0-9]+', names[0]):
        raise ValueError('ambiguous native thermal sensor')
    return names[0]


def ancestors(paths):
    result = set(paths)
    for path in paths:
        parent = str(Path(path).parent)
        while parent.startswith('/sys/fs/cgroup'):
            result.add(parent)
            if parent == '/sys/fs/cgroup':
                break
            parent = str(Path(parent).parent)
    return result


def sample_paths(bound, paths, hwmon):
    roster = list(LIFETIME_PATHS + HOST_PATHS)
    roster += ['/sys/class/nvme/' + bound['controller'] + '/state']
    if hwmon:
        base = '/sys/class/nvme/' + bound['controller'] + '/device/hwmon/' + hwmon
        roster += [base + '/' + name for name in ('temp1_input', 'temp1_crit', 'temp1_alarm')]
    if bound.get('pci'):
        roster += ['/sys/bus/pci/devices/' + bound['pci'] + '/' + name for name in ('aer_dev_correctable', 'aer_dev_nonfatal', 'aer_dev_fatal')]
    roster += [base + '/' + name for base in sorted(paths) for name in CGROUP_FIELDS]
    return roster


def cgroup_paths(pod_uid):
    if not re.fullmatch(UUID, pod_uid):
        raise ValueError('unread native Pod UID')
    escaped = pod_uid.replace('-', '_')
    return ('/sys/fs/cgroup/kubepods/burstable/pod' + pod_uid,
            '/sys/fs/cgroup/kubepods.slice/kubepods-burstable.slice/kubepods-burstable-pod' + escaped + '.slice')


def read_argv(path, bound_device, cgroups, hwmon=None):
    allowed = set(HOST_PATHS + LIFETIME_PATHS)
    controller = bound_device.get('controller', '')
    if re.fullmatch(r'nvme[0-9]+', controller):
        allowed.add('/sys/class/nvme/' + controller + '/state')
        if hwmon and re.fullmatch(r'hwmon[0-9]+', hwmon):
            base = '/sys/class/nvme/' + controller + '/device/hwmon/' + hwmon
            allowed.update(base + '/' + key for key in ('name', 'temp1_input', 'temp1_crit', 'temp1_alarm'))
    pci = bound_device.get('pci')
    if pci and re.fullmatch(r'[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7]', pci):
        allowed.update('/sys/bus/pci/devices/' + pci + '/' + key for key in ('aer_dev_correctable', 'aer_dev_nonfatal', 'aer_dev_fatal'))
    roots = {'/sys/fs/cgroup', '/sys/fs/cgroup/kubepods', '/sys/fs/cgroup/kubepods/burstable',
             '/sys/fs/cgroup/kubepods.slice', '/sys/fs/cgroup/kubepods.slice/kubepods-burstable.slice'}
    for base in cgroups:
        if base not in roots and not re.fullmatch(r'/sys/fs/cgroup/kubepods/burstable/pod' + UUID, base) and not re.fullmatch(r'/sys/fs/cgroup/kubepods.slice/kubepods-burstable.slice/kubepods-burstable-pod' + UUID.replace('-', '_') + r'\.slice', base):
            raise ValueError('cgroup outside exact gate UID/ancestry roster')
    allowed.update(base + '/' + field for base in cgroups for field in CGROUP_FIELDS)
    # cgroups contains only paths derived from controlled Pod UIDs/ancestry.
    if path not in allowed or '..' in path.split('/'):
        raise ValueError('path outside fixed metadata roster')
    return ['read', path]


def byte_record(raw):
    return {'bytes': len(raw), 'sha256': hashlib.sha256(raw).hexdigest(),
            'base64': base64.b64encode(raw).decode('ascii')}


def decode_record(row):
    try:
        raw = base64.b64decode(row['base64'], validate=True)
    except (ValueError, KeyError, TypeError) as error:
        raise ValueError('unread byte record') from error
    if type(row.get('bytes')) is not int or row['bytes'] != len(raw) or row.get('sha256') != hashlib.sha256(raw).hexdigest():
        raise ValueError('byte conservation failed')
    return raw


def observation_bounds(readings):
    """One qualification of the original collection, shared with offline replay.

    The final thirty seconds belong to attachment publication. A complete
    ordinary field refusal is evidence; a truncated or unattempted field is
    a bound on the observation as a whole, including optional roster entries.
    """
    if not readings or len(readings) > MAX_READS:
        raise ValueError('observation roster bound reached or unread')
    first, previous, retained = None, None, 0
    for row in readings:
        if row['state'] == 'not-attempted' or row.get('stream_complete') is False:
            raise ValueError('observation contains an unattempted or incomplete reading')
        begin, end = row.get('begin_monotonic'), row.get('end_monotonic')
        if any(type(value) not in (int, float) or not math.isfinite(value) for value in (begin, end)):
            raise ValueError('observation clock unavailable')
        if end < begin or (previous is not None and begin < previous):
            raise ValueError('observation clock order differs')
        first = begin if first is None else first
        if end - begin > CALL_SECONDS or end - first > TOTAL_SECONDS - 30:
            raise ValueError('observation call or collection time bound reached')
        previous = end
        size = sum(len(decode_record(row[key])) for key in ('stdout', 'stderr') if key in row)
        retained += size
        if size > SCALAR_BYTES or retained > TOTAL_BYTES:
            raise ValueError('observation scalar or total byte bound reached')


def native_command(argv, timeout):
    """Keep both bounded streams; stop only the process group this call creates."""
    process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    streams = {process.stdout: bytearray(), process.stderr: bytearray()}
    selector = selectors.DefaultSelector()
    for stream in streams:
        selector.register(stream, selectors.EVENT_READ)
    deadline = time.monotonic() + timeout
    complete = True
    try:
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                complete = False
                break
            ready = selector.select(min(remaining, 0.1))
            for key, _ in ready:
                block = os.read(key.fd, 8192)
                if not block:
                    selector.unregister(key.fileobj)
                    continue
                room = SCALAR_BYTES - sum(map(len, streams.values()))
                streams[key.fileobj].extend(block[:room])
                if len(block) > room:
                    complete = False
                    break
            if not complete:
                break
        if not complete:
            os.killpg(process.pid, signal.SIGKILL)
        code = process.wait(timeout=max(0.01, deadline - time.monotonic()))
    except (subprocess.TimeoutExpired, ProcessLookupError):
        complete = False
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        code = process.wait(timeout=1)
    finally:
        selector.close()
        for stream in streams:
            stream.close()
    return code, bytes(streams[process.stdout]), bytes(streams[process.stderr]), complete


class Recorder:
    def __init__(self, execute=native_command, clock=time.monotonic, wall=None, limit=MAX_READS, sink=None):
        self.execute, self.clock = execute, clock
        self.wall = wall or (lambda: datetime.datetime.now(datetime.timezone.utc).isoformat())
        self.begin, self.rows, self.bytes = clock(), [], 0
        self.limit = limit
        self.sink = sink or (lambda _: None)

    def call(self, operation, argv):
        if len(self.rows) >= self.limit:
            raise ValueError('observation roster bound reached; no further read attempted')
        start = self.clock()
        row = {'operation': operation, 'argv': argv, 'begin': self.wall(), 'begin_monotonic': start}
        self.sink({'event': 'started', **row})
        if start - self.begin >= TOTAL_SECONDS - 30 or self.bytes >= TOTAL_BYTES:
            row.update(state='not-attempted', reason='observation bound reached')
        else:
            try:
                code, out, err, complete = self.execute(argv, min(CALL_SECONDS, TOTAL_SECONDS - 30 - (start - self.begin)))
                room = TOTAL_BYTES - self.bytes
                if len(out) + len(err) > room:
                    out, err, complete = out[:room], err[:max(0, room - len(out))], False
                self.bytes += len(out) + len(err)
                row.update(state='read' if code == 0 and complete else 'unavailable', exit=code,
                           stream_complete=complete, stdout=byte_record(out), stderr=byte_record(err))
            except OSError as error:
                row.update(state='unavailable', error_type=type(error).__name__, errno=error.errno)
        row.update(end=self.wall(), end_monotonic=self.clock())
        self.rows.append(row)
        self.sink({'event': 'finished', **row})
        return row


def text(row):
    if row['state'] != 'read' or type(row.get('exit')) is not int or row['exit'] != 0 or row.get('stream_complete') is not True:
        raise ValueError('native reading unavailable: ' + row['operation'])
    begin, end = row.get('begin_monotonic'), row.get('end_monotonic')
    if any(type(value) not in (int, float) or not math.isfinite(value) for value in (begin, end)) or end < begin:
        raise ValueError('native read clock unavailable: ' + row['operation'])
    return decode_record(row['stdout']).decode('utf-8', errors='strict')


def disk_rows(raw):
    result = {}
    for line in raw.splitlines():
        parts = line.split()
        if len(parts) < 14:
            raise ValueError('short diskstats row')
        if parts[2] in result:
            raise ValueError('duplicate diskstats device')
        numbers = [int(n) for n in parts[3:]]
        if any(n < 0 for n in numbers):
            raise ValueError('negative diskstats')
        result[parts[2]] = (parts[0] + ':' + parts[1], numbers)
    return result


def disk_delta(before, after, name, seconds):
    if type(seconds) not in (int, float) or not math.isfinite(seconds) or seconds <= 0:
        raise ValueError('unread elapsed time')
    a, b = disk_rows(before)[name], disk_rows(after)[name]
    if a[0] != b[0] or len(a[1]) != len(b[1]):
        raise ValueError('device identity changed')
    delta = [y - x for x, y in zip(a[1], b[1])]
    if any(v < 0 for i, v in enumerate(delta) if i != 8):
        raise ValueError('counter reset or wrap')
    return {'device': name, 'major_minor': a[0], 'seconds': seconds,
            'read_bytes': delta[2] * 512, 'write_bytes': delta[6] * 512,
            'read_await_ms': delta[3] / delta[0] if delta[0] else None,
            'write_await_ms': delta[7] / delta[4] if delta[4] else None,
            'inflight_before': a[1][8], 'inflight_after': b[1][8]}


def io_delta(before, after, major_minor):
    def parse(raw):
        result = {}
        for line in raw.splitlines():
            parts = line.split()
            if parts[0] in result:
                raise ValueError('duplicate cgroup device')
            result[parts[0]] = unique((key, int(value)) for key, value in (p.split('=') for p in parts[1:]))
        return result
    a, b = parse(before), parse(after)
    if major_minor not in a or major_minor not in b:
        raise ValueError('whole-device accounting row unavailable')
    result = {}
    for key, label in (('rbytes', 'accounted_read_bytes'), ('wbytes', 'accounted_write_bytes'), ('rios', 'accounted_reads'), ('wios', 'accounted_writes')):
        x, y = a[major_minor][key], b[major_minor][key]
        if x < 0 or y < x:
            raise ValueError('accounting counter reset or wrap')
        result[label] = y - x
    return result


# These native projections deliberately do not export Pod env/args or secrets.
PODS = r'jsonpath={"continue\t"}{.metadata.continue}{"\n"}{range .items[*]}{.metadata.name}{"\t"}{.metadata.uid}{"\t"}{.status.phase}{"\t"}{.spec.nodeName}{"\t"}{.metadata.ownerReferences[?(@.controller==true)].kind}{"\t"}{.metadata.ownerReferences[?(@.controller==true)].name}{"\t"}{.metadata.ownerReferences[?(@.controller==true)].uid}{"\t"}{.status.containerStatuses[?(@.name=="gate")].containerID}{"\t"}{.status.containerStatuses[?(@.name=="gate")].restartCount}{"\n"}{end}'
JOBS = r'jsonpath={"continue\t"}{.metadata.continue}{"\n"}{range .items[*]}{.metadata.name}{"\t"}{.metadata.uid}{"\t"}{.metadata.labels.boss\.dev/packet}{"\n"}{end}'


def cohort(pods, jobs, node):
    def lines(raw, count):
        rows = raw.splitlines()
        if not rows or rows[0] != 'continue\t':
            raise ValueError('native census missing completion control')
        result = [line.split('\t') for line in rows[1:]]
        if any(len(row) != count for row in result):
            raise ValueError('malformed native census')
        return result
    owners = {}
    for name, uid, packet in lines(jobs, 3):
        if name in owners or not re.fullmatch(DNS, name) or not re.fullmatch(UUID, uid) or not re.fullmatch(UUID, packet):
            raise ValueError('unread gate Job identity')
        owners[name] = (uid, packet)
    result = []
    for name, uid, phase, actual_node, kind, owner, owner_uid, container, restarts in lines(pods, 9):
        if phase != 'Running':
            continue
        if actual_node != node or kind != 'Job' or owners.get(owner, (None,))[0] != owner_uid or not re.fullmatch(UUID, uid) or not re.fullmatch(DNS, name):
            raise ValueError('Pod and gate Job identities disagree')
        if not re.fullmatch(r'containerd://[0-9a-f]{64}', container) or not re.fullmatch(r'[0-9]+', restarts):
            raise ValueError('gate container lifetime unavailable')
        result.append({'pod': name, 'pod_uid': uid, 'job': owner, 'job_uid': owner_uid,
                       'packet': owners[owner][1], 'container_id': container, 'restarts': int(restarts)})
    if len({row['pod_uid'] for row in result}) != len(result):
        raise ValueError('duplicate Pod identity')
    return sorted(result, key=lambda row: row['pod_uid'])


class Collector:
    def __init__(self, recorder, kubectl, talos, registry, sleep=time.sleep):
        self.recorder, self.kubectl, self.talos, self.registry = recorder, kubectl, talos, registry
        self.bound, self.address, self.node, self.cgroups, self.hwmon = {}, None, None, set(), None
        self.sleep = sleep

    def kube(self, op, *args):
        return self.recorder.call(op, self.kubectl + list(args) + ['--request-timeout=20s'])

    def native(self, op, *args):
        if not self.address:
            raise ValueError('no bound native address')
        return self.recorder.call(op, self.talos + ['-n', self.address] + list(args))

    def reading(self, op, path):
        return self.native(op, *read_argv(path, self.bound, self.cgroups, self.hwmon))

    def identify(self, prefix):
        pvc = one_json(text(self.kube(prefix + '.pvc', 'get', 'pvc', 'gate', '-n', 'boss-dev', '-o', 'json')))
        pv_name = pvc['spec']['volumeName']
        if not re.fullmatch(DNS, pv_name):
            raise ValueError('PV name outside native DNS shape')
        pv = one_json(text(self.kube(prefix + '.pv', 'get', 'pv', pv_name, '-o', 'json')))
        shape = topology(pvc, pv)
        registry = one_json(text(self.recorder.call(prefix + '.registry', ['bash', self.registry, shape['node']])))
        address, uid = worker(shape, registry)
        self.address, self.node = address, shape['node']
        volumes = json_stream(text(self.native(prefix + '.volume', 'get', 'volumestatus', 'u-gate', '-o', 'json')))
        disks = json_stream(text(self.native(prefix + '.disks', 'get', 'disks', '-o', 'json')))
        bound = device(volumes, disks, address)
        return {**shape, 'node_uid': uid, 'address': address, 'device': bound}

    def gates(self, prefix):
        pods = text(self.kube(prefix + '.pods', 'get', 'pods', '-n', 'boss-dev', '-l', 'app=gate-runner', '--field-selector', 'spec.nodeName=' + self.node, '-o', PODS))
        jobs = text(self.kube(prefix + '.jobs', 'get', 'jobs', '-n', 'boss-dev', '-l', 'app=gate-runner', '-o', JOBS))
        return cohort(pods, jobs, self.node)

    def discover_hwmon(self):
        base = '/sys/class/nvme/' + self.bound['controller'] + '/device/hwmon'
        raw = text(self.native('sensor.directory', 'list', base, '--depth=1', '--type=d'))
        self.hwmon = sensor_directory(raw)
        try:
            if text(self.reading('sensor.name', base + '/' + self.hwmon + '/name')).strip() != 'nvme':
                raise ValueError('thermal sensor is not NVMe')
        except (ValueError, KeyError, TypeError):
            self.hwmon = None
            raise

    def collect(self):
        record = {'schema': SCHEMA, 'claim': 'gate-storage-observation', 'source_contract': 'Talos-v1.13.8/Linux-counter-ABI',
                  'availability': {'media_health': 'unavailable: no selected SMART API',
                                   'cgroup_generation': 'unavailable: selected Read/List API does not expose a stable inode/generation',
                                   'writer_attribution': 'unavailable: kernel accounting boundary not qualified'}, 'samples': []}
        try:
            selected_client = client_path(self.talos)
            self.recorder.call('before.client-sha256', ['sha256sum', '--', selected_client])
            record['identity_before'] = self.identify('before')
            self.bound = record['identity_before']['device']
            self.native('versions', 'version')
            gates = self.gates('before')
            record['cohort_before'] = gates
            try:
                self.discover_hwmon()
            except (ValueError, KeyError, TypeError) as error:
                record['availability']['thermal_sensor'] = str(error)
            paths = []
            # Probe exact native UID paths once, not a host-wide recursive walk.
            for gate in gates:
                candidates = cgroup_paths(gate['pod_uid'])
                for candidate in candidates:
                    self.cgroups.add(candidate)
                    row = self.reading('cgroup-discovery:' + candidate, candidate + '/io.stat')
                    if row['state'] == 'read':
                        paths.append(candidate)
                        break
                else:
                    record['availability'][gate['pod_uid']] = 'native cgroup path unavailable'
            paths = ancestors(paths)
            self.cgroups.update(paths)
            for index in range(2):
                prefix = 'sample' + str(index)
                if index:
                    remaining = 15 - (self.recorder.clock() - record['samples'][0]['begin_monotonic'])
                    if remaining > 0:
                        self.sleep(min(remaining, max(0, TOTAL_SECONDS - 30 - (self.recorder.clock() - self.recorder.begin))))
                sample = {'begin_monotonic': self.recorder.clock(), 'operations': []}
                roster = sample_paths(self.bound, paths, self.hwmon)
                for path in roster:
                    operation = prefix + ':' + path
                    self.reading(operation, path)
                    sample['operations'].append(operation)
                for path in LIFETIME_PATHS[:2]:
                    operation = prefix + '.end:' + path
                    self.reading(operation, path)
                    sample['operations'].append(operation)
                record['samples'].append(sample)
            record['cohort_after'] = self.gates('after')
            record['identity_after'] = self.identify('after')
            self.recorder.call('after.client-sha256', ['sha256sum', '--', selected_client])
            record['state'] = 'observed' if record['identity_before'] == record['identity_after'] else 'unavailable'
            if record['state'] != 'observed':
                record['reason'] = 'native topology or device identity changed'
        except (ValueError, KeyError, TypeError, UnicodeError, OSError) as error:
            record.update(state='unavailable', reason=type(error).__name__ + ': ' + str(error))
        record['readings'] = self.recorder.rows
        try:
            observation_bounds(record['readings'])
        except (ValueError, KeyError, TypeError) as error:
            record.update(state='unavailable', reason='observation byte, roster or time bound reached: ' + str(error))
        return record


def derive(record):
    """No clock, I/O or classifications outside the supplied immutable record."""
    if record.get('schema') != SCHEMA:
        raise ValueError('unknown observation schema')
    rows = {}
    for row in record['readings']:
        if row['operation'] in rows:
            raise ValueError('duplicate recorded operation')
        rows[row['operation']] = row
        for field in ('stdout', 'stderr'):
            if field in row:
                decode_record(row[field])
    out = {'schema': SCHEMA, 'claim': 'gate-storage-observation-only', 'state': 'unavailable',
           'media_health': 'unavailable', 'writer_attribution': 'unavailable', 'cgroup_deltas': 'unavailable: native generation unverified',
           'inventory': {'total': len(rows), 'read': sum(r['state'] == 'read' for r in rows.values()),
                         'non_success': sum(r['state'] != 'read' for r in rows.values())}}
    try:
        observation_bounds(record['readings'])
        if record['state'] != 'observed' or record['identity_before'] != record['identity_after']:
            raise ValueError('identity not conserved')
        # A cached identity is a convenience, never another source of truth.
        # Replaying the native originals must establish the same relationship.
        kube_tail = ['get', 'pvc', 'gate', '-n', 'boss-dev', '-o', 'json', '--request-timeout=20s']
        kube_argv = rows['before.pvc']['argv']
        if not isinstance(kube_argv, list) or kube_argv[-len(kube_tail):] != kube_tail or len(kube_argv) <= len(kube_tail):
            raise ValueError('native Kubernetes command binding unavailable')
        kube = kube_argv[:-len(kube_tail)]
        version_argv = rows['versions']['argv']
        if not isinstance(version_argv, list) or len(version_argv) < 4 or version_argv[-3] != '-n' or version_argv[-1] != 'version':
            raise ValueError('native Talos command binding unavailable')
        talos = version_argv[:-3]
        registry_argv = rows['before.registry']['argv']
        if len(registry_argv) != 3 or registry_argv[0] != 'bash':
            raise ValueError('native registry command binding unavailable')
        before = replay_identity(rows, 'before', kube, talos, registry_argv[1])
        after = replay_identity(rows, 'after', kube, talos, registry_argv[1])
        if before != after or record['identity_before'] != before or record['identity_after'] != after:
            raise ValueError('cached target differs from complete native originals')
        cohorts = {}
        for prefix, identity in (('before', before), ('after', after)):
            native_cohort = replay_cohort(rows, prefix, kube, identity)
            if record['cohort_' + prefix] != native_cohort:
                raise ValueError('cached cohort differs from complete native originals')
            cohorts[prefix] = native_cohort
        selected_client = client_path(talos)
        hashes = []
        for prefix in ('before', 'after'):
            hashes.append(client_hash(rows[prefix + '.client-sha256'], selected_client))
        if hashes[0] != hashes[1]:
            raise ValueError('actual selected Talos client changed')
        address = before['address']
        expected = {'versions', 'before.client-sha256', 'after.client-sha256'}
        expected.update(prefix + '.' + field for prefix in ('before', 'after')
                        for field in ('pvc', 'pv', 'registry', 'volume', 'disks', 'pods', 'jobs'))
        sensor_base = '/sys/class/nvme/' + before['device']['controller'] + '/device/hwmon'
        expected.add('sensor.directory')
        directory = rows['sensor.directory']
        if directory.get('argv') != talos + ['-n', address, 'list', sensor_base, '--depth=1', '--type=d']:
            raise ValueError('native sensor directory binding differs')
        hwmon = None
        try:
            candidate = sensor_directory(text(directory))
        except (ValueError, KeyError, TypeError, UnicodeError):
            candidate = None
        if candidate:
            expected.add('sensor.name')
            name = rows['sensor.name']
            if name.get('argv') != talos + ['-n', address, 'read', sensor_base + '/' + candidate + '/name']:
                raise ValueError('native sensor name binding differs')
            try:
                if text(name).strip() == 'nvme':
                    hwmon = candidate
            except (ValueError, KeyError, TypeError, UnicodeError):
                pass
        paths = []
        for gate in cohorts['before']:
            for candidate in cgroup_paths(gate['pod_uid']):
                operation = 'cgroup-discovery:' + candidate
                expected.add(operation)
                row = rows[operation]
                if row.get('argv') != talos + ['-n', address, 'read', candidate + '/io.stat']:
                    raise ValueError('native cgroup discovery binding differs')
                if row['state'] == 'read':
                    text(row)
                    paths.append(candidate)
                    break
        paths = ancestors(paths)
        if len(record['samples']) != 2:
            raise ValueError('incomplete sample inventory')
        for index in range(2):
            prefix = 'sample' + str(index)
            operations = [prefix + ':' + path for path in sample_paths(before['device'], paths, hwmon)]
            operations += [prefix + '.end:' + path for path in LIFETIME_PATHS[:2]]
            if record['samples'][index]['operations'] != operations:
                raise ValueError('sample roster differs from native identities')
            expected.update(operations)
        if set(rows) != expected:
            raise ValueError('complete native operation inventory differs')
        for operation, row in rows.items():
            if operation.startswith(('sample0:', 'sample1:', 'sample0.end:', 'sample1.end:', 'cgroup-discovery:')):
                path = operation.split(':', 1)[1]
                if operation.startswith('cgroup-discovery:'):
                    path += '/io.stat'
                # Compare failed reads as well: an unavailable field still has
                # one fixed operation identity, not an arbitrary target.
                if row.get('argv') != talos + ['-n', address, 'read', path]:
                    raise ValueError('sample native target/path differs: ' + operation)
        versions = bound_text(rows['versions'], talos, ['-n', address, 'version'])
        tags = re.findall(r'^\s*Tag:\s*(\S+)\s*$', versions, re.MULTILINE)
        if not re.search(r'^Client:\s*$', versions, re.MULTILINE) or not re.search(r'^Server:\s*$', versions, re.MULTILINE) or tags != ['v1.13.8', 'v1.13.8']:
            raise ValueError('actual client/server version does not establish the pinned v1.13.8 contract')
        boot = []
        uptime = []
        for prefix in ('sample0', 'sample1'):
            first = text(rows[prefix + ':/proc/sys/kernel/random/boot_id']).strip()
            last = text(rows[prefix + '.end:/proc/sys/kernel/random/boot_id']).strip()
            if not re.fullmatch(UUID, first) or first != last:
                raise ValueError('native boot lifetime changed or unread')
            boot.append(first)
            if not text(rows[prefix + ':/proc/version']).startswith('Linux version '):
                raise ValueError('native kernel version unavailable')
            a = float(text(rows[prefix + ':/proc/uptime']).split()[0])
            b = float(text(rows[prefix + '.end:/proc/uptime']).split()[0])
            if not math.isfinite(a) or not math.isfinite(b) or a < 0 or b < a:
                raise ValueError('native uptime reset or unread')
            uptime.append((a, b))
        if boot[0] != boot[1] or uptime[1][0] < uptime[0][1]:
            raise ValueError('incompatible native lifetimes')
        a, b = rows['sample0:/proc/diskstats'], rows['sample1:/proc/diskstats']
        elapsed = (b['begin_monotonic'] + b['end_monotonic'] - a['begin_monotonic'] - a['end_monotonic']) / 2
        name = before['device']['whole']
        disk = disk_delta(text(a), text(b), name, elapsed)
        disk['timing_method'] = 'trusted-host call midpoints; individual call brackets retained, not atomic samples'
        out.update(state='observed', disk=disk, client_sha256=hashes[0],
                   cohort_stable=record['cohort_before'] == record['cohort_after'])
        memory = []
        for prefix in ('sample0', 'sample1'):
            fields = {}
            for line in text(rows[prefix + ':/proc/meminfo']).splitlines():
                parts = line.split()
                if parts[0] in ('Dirty:', 'Writeback:'):
                    if len(parts) != 3 or parts[2] != 'kB' or not re.fullmatch(r'[0-9]+', parts[1]) or parts[0] in fields:
                        raise ValueError('unread native dirty/writeback units')
                    fields[parts[0]] = int(parts[1]) * 1024
            memory.append({'dirty_bytes': fields['Dirty:'], 'writeback_bytes': fields['Writeback:']})
        pressure = {}
        for kind in ('io', 'cpu', 'memory'):
            def totals(prefix):
                found = {}
                for line in text(rows[prefix + ':/proc/pressure/' + kind]).splitlines():
                    parts = line.split()
                    values = dict(part.split('=') for part in parts[1:])
                    if parts[0] not in ('some', 'full') or parts[0] in found or not re.fullmatch(r'[0-9]+', values['total']):
                        raise ValueError('unread native pressure')
                    found[parts[0]] = int(values['total'])
                if 'some' not in found:
                    raise ValueError('pressure some counter unavailable')
                return found
            a, b = totals('sample0'), totals('sample1')
            if set(a) != set(b) or any(b[key] < value for key, value in a.items()):
                raise ValueError('pressure counter reset or changed schema')
            pressure[kind] = {key + '_stall_us': b[key] - value for key, value in a.items()}
        out.update(memory=memory, pressure=pressure)
    except (ValueError, KeyError, IndexError, TypeError, UnicodeError) as error:
        out['state'] = 'unavailable'
        out['reason'] = type(error).__name__ + ': ' + str(error)
    return out


def write_json(path, body):
    raw = (json.dumps(body, sort_keys=True, separators=(',', ':'), allow_nan=False) + '\n').encode()
    path.write_bytes(raw)
    return raw


def attach_record(work, request, record, execute, remaining):
    """The existing attach door verifies stored bytes by readback; never a digest-only substitute."""
    raw = write_json(work / 'observation.json', record)
    manifest = {'schema': SCHEMA, 'bytes': len(raw), 'sha256': hashlib.sha256(raw).hexdigest(), 'parts': []}
    for index, start in enumerate(range(0, len(raw), 1024 * 1024)):
        part = work / ('observation-part-' + str(index) + '.json.bytes')
        block = raw[start:start + 1024 * 1024]
        part.write_bytes(block)
        seconds = min(CALL_SECONDS, remaining())
        if seconds <= 0:
            raise ValueError('attachment deadline reached; original retained at ' + str(work))
        code, out, err, complete = execute(['boss', 'attach', request, str(part)], seconds)
        receipt = {'exit': code, 'stdout': byte_record(out), 'stderr': byte_record(err), 'complete': complete}
        write_json(work / ('attachment-' + str(index) + '.json'), receipt)
        if code or not complete:
            raise ValueError('attachment not proven; original retained at ' + str(work))
        manifest['parts'].append({'name': part.name, **byte_record(block), 'receipt': receipt})
        # The manifest links hashes and native attach receipts, not another base64 copy.
        del manifest['parts'][-1]['base64']
    manifest['derivation'] = derive(record)
    path = work / 'observation-manifest.json'
    write_json(path, manifest)
    seconds = min(CALL_SECONDS, remaining())
    if seconds <= 0:
        raise ValueError('manifest attachment deadline reached; original retained at ' + str(work))
    code, out, err, complete = execute(['boss', 'attach', request, str(path)], seconds)
    write_json(work / 'manifest-attachment.json', {'exit': code, 'stdout': byte_record(out), 'stderr': byte_record(err), 'complete': complete})
    if code or not complete:
        raise ValueError('manifest attachment not proven; original retained at ' + str(work))
    return {'manifest': byte_record(path.read_bytes()), 'native_attach_receipt': out.decode('utf-8', errors='replace'),
            'parts': len(manifest['parts']), 'derivation': manifest['derivation']}


def main():
    if len(sys.argv) == 3 and sys.argv[1] == '--replay':
        record = one_json(Path(sys.argv[2]).read_text())
        print(json.dumps(derive(record), sort_keys=True))
        return 0
    if len(sys.argv) != 1:
        print('read-gate-storage: REFUSED — no native arguments accepted', file=sys.stderr)
        return 78
    request = os.environ.get('OPS_REQUEST_ID', '')
    if not re.fullmatch(UUID, request):
        print('read-gate-storage: REFUSED — native ops request identity required', file=sys.stderr)
        return 78
    # Preserve local originals on every failure, rather than deleting the only copy.
    work = Path(tempfile.mkdtemp(prefix='boss-gate-storage-' + request + '-'))
    os.chmod(work, 0o700)
    start = time.monotonic()
    journal = work / 'readings.jsonl'
    def append(row):
        with journal.open('ab') as stream:
            stream.write((json.dumps(row, sort_keys=True) + '\n').encode())
            stream.flush()
    recorder = Recorder(sink=append)
    try:
        kube = shlex.split(os.environ['GATE_STORAGE_KUBECTL'])
        talos = shlex.split(os.environ['GATE_STORAGE_TALOS'])
        if not kube or not talos:
            raise ValueError('existing native doors unavailable')
        collector = Collector(recorder, kube, talos, str(Path(__file__).with_name('gate-storage-registry.sh')))
        record = collector.collect()
        record.update(request=request, adapter_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest())
        write_json(work / 'observation.json', record)
        published = attach_record(work, request, record, native_command,
                                  lambda: TOTAL_SECONDS - (time.monotonic() - start))
        # Manifest bytes are already in the file store; stdout stays below the ops cap.
        published['manifest'].pop('base64')
        print(json.dumps({'claim': 'gate-storage-observation-only', 'retained_original': str(work), **published}, sort_keys=True))
        return 0 if published['derivation']['state'] == 'observed' else 1
    except (ValueError, KeyError, TypeError, OSError) as error:
        print(json.dumps({'state': 'unavailable', 'claim': 'gate-storage-observation-only',
                          'error': type(error).__name__ + ': ' + str(error), 'retained_original': str(work)}, sort_keys=True))
        return 1


if __name__ == '__main__':
    sys.exit(main())
