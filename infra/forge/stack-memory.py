#!/usr/bin/env python3
"""Passive stack memory evidence, never a recommendation or a rollout."""
import datetime
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import sys
import tempfile

_spec = importlib.util.spec_from_file_location(
    'bounded_stack_observation', Path(__file__).with_name('gate-storage.py'))
bounded = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bounded)
UUID = r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}'
NAME = r'[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?'
CREATION_TO_START_SECONDS = 5


def instant(value):
    if not isinstance(value, str) or not value.endswith(('Z', '+00:00')):
        raise ValueError('native UTC sample time unavailable')
    return datetime.datetime.fromisoformat(value.replace('Z', '+00:00'))


def numeric(value, positive=False):
    if type(value) is not int or value < 0 or (positive and value == 0):
        raise ValueError('native memory byte counter unavailable')
    return value


def memory_fields(value, now, positive=False):
    if not isinstance(value, dict):
        raise ValueError('native memory object unavailable')
    age = (instant(now) - instant(value.get('time'))).total_seconds()
    if not 0 <= age <= 60:
        raise ValueError('native memory sample stale or future')
    return {'sample_at': value['time'],
            'working_set_bytes': numeric(value.get('workingSetBytes'), positive),
            'usage_bytes': numeric(value.get('usageBytes'), positive)}


def quantity(value):
    match = re.fullmatch(r'([0-9]+)(Ki|Mi|Gi|Ti)?', value)
    if not match:
        raise ValueError('native memory quantity unavailable')
    return int(match[1]) * {'Ki': 1024, 'Mi': 1024**2, 'Gi': 1024**3,
                          'Ti': 1024**4, None: 1}[match[2]]


def memory(summary, node, pod, uid, started, now, namespace='boss'):
    if summary['node']['nodeName'] != node:
        raise ValueError('summary node identity differs')
    matches = [p for p in summary['pods'] if p.get('podRef') == {
        'namespace': namespace, 'name': pod, 'uid': uid}]
    if len(matches) != 1:
        raise ValueError('exact stack Pod absent or ambiguous in summary')
    containers = [c for c in matches[0]['containers'] if c.get('name') == 'boss']
    if len(containers) != 1:
        raise ValueError('stack container absent or ambiguous')
    # Kubelet summary startTime is CRI creation, not running.startedAt.
    # A larger gap stays unavailable; before/after identity binds the read.
    delay = (instant(started) - instant(containers[0].get('startTime'))).total_seconds()
    if not 0 <= delay <= CREATION_TO_START_SECONDS:
        raise ValueError('stack container lifetime differs')
    # Narrow kubelet summaries fill absent cAdvisor container values with zero.
    container = memory_fields(containers[0]['memory'], now, positive=True)
    container['rss_bytes'] = numeric(containers[0]['memory'].get('rssBytes'), positive=True)
    host = memory_fields(summary['node']['memory'], now)
    host['available_bytes'] = numeric(summary['node']['memory'].get('availableBytes'))
    return {'container': container, 'node': host}


# Projections omit all environment, arguments, annotations and credentials.
DEPLOY = r'jsonpath={.metadata.uid}{"\t"}{.metadata.generation}{"\t"}{.spec.selector.matchLabels.app}{"\t"}{.spec.replicas}{"\n"}'
PODS = r'jsonpath={"continue\t"}{.metadata.continue}{"\n"}{range .items[*]}{.metadata.name}{"\t"}{.metadata.uid}{"\t"}{.spec.nodeName}{"\t"}{.metadata.ownerReferences[?(@.controller==true)].kind}{"\t"}{.metadata.ownerReferences[?(@.controller==true)].name}{"\t"}{.metadata.ownerReferences[?(@.controller==true)].uid}{"\t"}{.status.phase}{"\t"}{.metadata.deletionTimestamp}{"\t"}{.status.containerStatuses[?(@.name=="boss")].containerID}{"\t"}{.status.containerStatuses[?(@.name=="boss")].restartCount}{"\t"}{.status.containerStatuses[?(@.name=="boss")].state.running.startedAt}{"\t"}{.spec.containers[?(@.name=="boss")].resources.requests.memory}{"\t"}{.spec.containers[?(@.name=="boss")].resources.limits.memory}{"\n"}{end}'
REPLICA = r'jsonpath={.metadata.uid}{"\t"}{.metadata.ownerReferences[?(@.controller==true)].kind}{"\t"}{.metadata.ownerReferences[?(@.controller==true)].name}{"\t"}{.metadata.ownerReferences[?(@.controller==true)].uid}{"\n"}'
NODE = r'jsonpath={.metadata.name}{"\t"}{.metadata.uid}{"\t"}{.status.capacity.memory}{"\t"}{.status.allocatable.memory}{"\n"}'


def fields(raw, count):
    lines = raw.splitlines()
    if len(lines) != 1 or len(lines[0].split('\t')) != count:
        raise ValueError('native identity row absent or ambiguous')
    return lines[0].split('\t')


def identity(deployment, pods, replica):
    duid, generation, label, replicas = fields(deployment, 4)
    lines = pods.splitlines()
    if len(lines) != 2 or lines[0] != 'continue\t':
        raise ValueError('stack Pod roster incomplete or not exactly one')
    p = fields(lines[1], 13)
    name, uid, node, kind, owner, owner_uid, phase, deletion, container, restarts, started, request, limit = p
    ruid, rkind, rname, parent = fields(replica, 4)
    if (label != 'boss' or replicas != '1' or not generation.isdecimal()
            or kind != 'ReplicaSet' or (ruid, rkind, rname, parent) != (owner_uid, 'Deployment', 'boss', duid)
            or phase != 'Running' or deletion or node not in ('cp-1', 'cp-2', 'cp-3', 'w-1')
            or not all(re.fullmatch(UUID, v) for v in (duid, uid, owner_uid))
            or not all(re.fullmatch(NAME, v) for v in (name, owner))
            or not re.fullmatch(r'containerd://[0-9a-f]{64}', container)
            or not restarts.isdecimal() or not request or not limit):
        raise ValueError('stack ownership or native lifetime unavailable')
    instant(started)
    return {'deployment_uid': duid, 'deployment_generation': int(generation),
            'replicaset': owner, 'replicaset_uid': owner_uid, 'pod': name, 'pod_uid': uid,
            'node': node, 'container_id': container, 'restarts': int(restarts),
            'started_at': started, 'request': request, 'limit': limit,
            'request_bytes': quantity(request), 'limit_bytes': quantity(limit)}


def collect(recorder, kubectl, namespace='boss', target='cp-3'):
    first_reading = len(recorder.rows)
    result = {'schema': 1, 'claim': 'stack-memory-observation-only', 'state': 'unavailable',
              'meaning': 'kubelet point samples, not lifetime peak, recommended limit, free capacity or proof of a stall cause'}
    def read(name, args):
        return bounded.text(recorder.call(name, kubectl + args + ['--request-timeout=20s']))

    def bind(prefix):
        deploy = read(namespace + '.' + prefix + '.deployment', ['get', 'deployment', 'boss', '-n', namespace, '-o', DEPLOY])
        pods = read(namespace + '.' + prefix + '.pods', ['get', 'pods', '-n', namespace, '-l', 'app=boss', '--chunk-size=64', '-o', PODS])
        lines = pods.splitlines()
        if len(lines) != 2 or lines[0] != 'continue\t':
            raise ValueError('stack Pod roster incomplete or not exactly one')
        owner = fields(lines[1], 13)[4]
        if not re.fullmatch(NAME, owner):
            raise ValueError('ReplicaSet name unavailable')
        replica = read(namespace + '.' + prefix + '.replicaset', ['get', 'replicaset', owner, '-n', namespace, '-o', REPLICA])
        return identity(deploy, pods, replica)

    try:
        before = bind('before')
        result['identity_before'] = before
        samples = []
        for node in dict.fromkeys((before['node'], target)):
            raw = read(node + '.node', ['get', 'node', node, '-o', NODE])
            name, uid, capacity, allocatable = fields(raw, 4)
            if name != node or not re.fullmatch(UUID, uid) or not capacity or not allocatable:
                raise ValueError('node capacity identity unavailable')
            row = recorder.call(node + '.summary', kubectl + ['get', '--raw',
                '/api/v1/nodes/' + node + '/proxy/stats/summary?only_cpu_and_memory=true', '--request-timeout=20s'])
            summary = bounded.one_json(bounded.text(row))
            now = row['end']
            if summary['node']['nodeName'] != node:
                raise ValueError('node summary identity differs')
            # All fields remain point observations; no capacity subtraction
            # turns allocatable minus a working set into a safe pod limit.
            host = memory_fields(summary['node']['memory'], now)
            host['available_bytes'] = numeric(summary['node']['memory'].get('availableBytes'))
            sample = {'node': node, 'node_uid': uid, 'capacity': capacity,
                      'allocatable': allocatable, 'capacity_bytes': quantity(capacity),
                      'allocatable_bytes': quantity(allocatable), 'memory': host}
            if node == before['node']:
                sample['stack'] = memory(summary, node, before['pod'], before['pod_uid'],
                                         before['started_at'], now, namespace)['container']
            if read(node + '.node-after', ['get', 'node', node, '-o', NODE]) != raw:
                raise ValueError('node identity or capacity changed during observation')
            samples.append(sample)
        after = bind('after')
        result['identity_after'] = after
        if before != after:
            raise ValueError('stack changed while observing')
        result.update(state='observed', samples=samples)
    except (ValueError, KeyError, TypeError, AttributeError, UnicodeError, OSError) as error:
        result['error'] = type(error).__name__ + ': ' + str(error)
    result['readings'] = recorder.rows[first_reading:]
    # These are retained bytes, not the unknown full size of an incomplete
    # response. The existing recorder shares one limit across both streams.
    result['call_bytes'] = []
    for row in result['readings']:
        out, err = row.get('stdout', {}).get('bytes'), row.get('stderr', {}).get('bytes')
        total = out + err if type(out) is int and type(err) is int else None
        result['call_bytes'].append({
            'operation': row['operation'], 'retained_stdout_bytes': out,
            'retained_stderr_bytes': err, 'retained_total_bytes': total,
            'shared_stream_limit_bytes': bounded.SCALAR_BYTES,
            'remaining_per_call_retained_bytes': bounded.SCALAR_BYTES - total if total is not None else None,
            'stream_complete': row.get('stream_complete')})
    return result


def main(recorder_factory=bounded.Recorder):
    if len(sys.argv) != 1 or not re.fullmatch(UUID, os.environ.get('OPS_REQUEST_ID', '')):
        print('read-stack-memory: REFUSED — no arguments and a native ops request required', file=sys.stderr)
        return 78
    request = os.environ['OPS_REQUEST_ID']
    work = Path(tempfile.mkdtemp(prefix='boss-stack-memory-' + request + '-'))
    os.chmod(work, 0o700)
    def retain(row):
        with (work / 'readings.jsonl').open('a') as stream:
            stream.write(json.dumps(row, sort_keys=True) + '\n')
    # Two fixed instances share the original total time/byte budget. A
    # partial or unschedulable instance stays unavailable, never a zero.
    recorder = recorder_factory(sink=retain, limit=25)
    try:
        kubectl = shlex.split(os.environ['STACK_MEMORY_KUBECTL'])
        if not kubectl:
            raise ValueError('existing native door unavailable')
        instances = {namespace: collect(recorder, kubectl, namespace, target)
                     for namespace, target in (('boss', 'cp-3'), ('boss-playground', 'w-1'))}
        result = {'schema': 1, 'claim': 'stack-memory-observation-only',
                  'state': 'observed' if all(v['state'] == 'observed' for v in instances.values()) else 'unavailable',
                  'instances': instances, 'readings': recorder.rows,
                  'meaning': 'per-instance point observations, not measured limits or aggregate capacity safety'}
        result['request'] = request
        result['source'] = {p.name: bounded.byte_record(p.read_bytes())['sha256'] for p in
                            (Path(__file__), Path(bounded.__file__), Path(__file__).with_name('read-stack-memory.sh'))}
        path = work / 'observation.json'
        path.write_text(json.dumps(result, sort_keys=True) + '\n')
        if path.stat().st_size > 2_096_128:
            raise ValueError('observation attachment bound reached; original retained')
        receipt = recorder.call('attach-observation', ['boss', 'attach', request, str(path)])
        print(json.dumps({'state': result['state'], 'claim': result['claim'],
                          'errors': {namespace: value['error'] for namespace, value in instances.items()
                                     if 'error' in value}, 'retained_original': str(work),
                          'call_bytes': {namespace: value.get('call_bytes', [])
                                         for namespace, value in instances.items()},
                          'attachment_receipt': bounded.text(receipt)}, sort_keys=True))
        return 0 if result['state'] == 'observed' else 1
    except (ValueError, KeyError, TypeError, OSError) as error:
        print(json.dumps({'state': 'unavailable', 'error': str(error), 'retained_original': str(work)}))
        return 1


if __name__ == '__main__':
    sys.exit(main())
