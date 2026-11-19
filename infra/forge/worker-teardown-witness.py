#!/usr/bin/env python3
"""Read-only physical-return witness. Logical release is not teardown."""
import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import sys
import tempfile
import time
from urllib.parse import urlencode
import uuid

_spec = importlib.util.spec_from_file_location('teardown_retention', Path(__file__).with_name('volume-snapshots.py'))
retention = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(retention)
bounded = retention.bounded
MAX_PAGES = 32
MAX_ROWS = 512


def validate(target):
    if not isinstance(target, dict) or set(target) != {
            'namespace', 'job_uid', 'pvc_uid', 'run_id', 'acquisition',
            'generation', 'pool', 'pool_version', 'amount', 'reclaimer'}:
        raise ValueError('unread frozen target')
    if not isinstance(target['namespace'], str) or not re.fullmatch(bounded.DNS, target['namespace']):
        raise ValueError('invalid frozen namespace')
    for key in ('job_uid', 'pvc_uid', 'run_id', 'acquisition'):
        if not isinstance(target[key], str) or not re.fullmatch(bounded.UUID, target[key]) or uuid.UUID(target[key]).int == 0:
            raise ValueError('invalid frozen ' + key)
    for key in ('generation', 'pool_version'):
        if type(target[key]) is not int or target[key] < 1:
            raise ValueError('invalid frozen ' + key)
    for key, pattern in [('pool', r'[a-z0-9][a-z0-9:_-]{0,127}'),
                         ('amount', r'(?:0|[1-9][0-9]*)(?:\.[0-9]+)?'),
                         ('reclaimer', r'[a-zA-Z0-9][a-zA-Z0-9:_./-]{0,255}')]:
        if not isinstance(target[key], str) or not re.fullmatch(pattern, target[key]):
            raise ValueError('invalid frozen ' + key)


def control(recorder, kubectl, namespace, phase):
    result = {}
    for label, kind, args in (
            ('namespace', 'Namespace', ['get', 'namespace', namespace]),
            ('service_account', 'ServiceAccount', ['get', 'serviceaccount', 'default', '-n', namespace])):
        row = recorder.call(phase + '.' + label, kubectl + args + ['-o', 'json', '--request-timeout=20s'])
        value = bounded.one_json(bounded.text(row))
        if not isinstance(value, dict) or not isinstance(value.get('metadata'), dict):
            raise ValueError('known-positive ' + label + ' identity unavailable')
        meta = value.get('metadata', {})
        if (value.get('apiVersion') != 'v1' or value.get('kind') != kind
                or meta.get('name') != (namespace if label == 'namespace' else 'default')
                or (label == 'service_account' and meta.get('namespace') != namespace)
                or not isinstance(meta.get('uid'), str) or not re.fullmatch(bounded.UUID, meta['uid'])
                or uuid.UUID(meta['uid']).int == 0
                or not isinstance(meta.get('resourceVersion'), str) or not meta['resourceVersion']
                or meta.get('deletionTimestamp') is not None):
            raise ValueError('known-positive ' + label + ' identity unavailable')
        result[label] = meta['uid']
    return result


def inventory(recorder, kubectl, namespace, plural, expected_uid):
    api, kind = ('batch/v1', 'JobList') if plural == 'jobs' else ('v1', 'PersistentVolumeClaimList')
    base = '/apis/batch/v1/' if plural == 'jobs' else '/api/v1/'
    resource = 'jobs' if plural == 'jobs' else 'persistentvolumeclaims'
    cursor, version, seen_names, seen_uids, cursors = '', None, set(), set(), set()
    for page in range(MAX_PAGES):
        query = {'limit': 16}
        if cursor:
            query['continue'] = cursor
        url = base + 'namespaces/' + namespace + '/' + resource + '?' + urlencode(query)
        row = recorder.call(plural + '.' + str(page), kubectl + ['get', '--raw', url, '--request-timeout=20s'])
        value = bounded.one_json(bounded.text(row))
        if not isinstance(value, dict) or not isinstance(value.get('metadata'), dict):
            raise ValueError(plural + ': unread native list metadata')
        meta = value.get('metadata', {})
        rv = meta.get('resourceVersion')
        if (value.get('apiVersion') != api or value.get('kind') != kind
                or not isinstance(value.get('items'), list)
                or not isinstance(rv, str) or not rv or (version is not None and version != rv)):
            raise ValueError(plural + ': incomplete or changed native list')
        version = rv
        for item in value['items']:
            if (not isinstance(item, dict) or not isinstance(item.get('metadata'), dict)
                    or item.get('apiVersion', api) != api
                    or item.get('kind', kind.removesuffix('List')) != kind.removesuffix('List')):
                raise ValueError(plural + ': unread native item shape or type')
            identity = item.get('metadata', {})
            name, uid = identity.get('name'), identity.get('uid')
            if (identity.get('namespace') != namespace or not isinstance(name, str)
                    or not re.fullmatch(r'[a-z0-9][a-z0-9.-]{0,252}', name)
                    or not isinstance(uid, str) or not re.fullmatch(bounded.UUID, uid)
                    or uuid.UUID(uid).int == 0
                    or uid in seen_uids or name in seen_names):
                raise ValueError(plural + ': unread or duplicate native identity')
            seen_names.add(name)
            seen_uids.add(uid)
            if len(seen_uids) > MAX_ROWS:
                raise ValueError(plural + ': row bound reached')
        cursor = meta.get('continue', '')
        remaining = meta.get('remainingItemCount', 0)
        if (not isinstance(cursor, str) or len(cursor) > 4096
                or type(remaining) is not int or remaining < 0):
            raise ValueError(plural + ': unread continuation')
        if not cursor:
            if remaining:
                raise ValueError(plural + ': remaining rows without continuation')
            if expected_uid in seen_uids:
                raise ValueError(plural + ': frozen UID still present')
            return {'complete': True, 'count': len(seen_uids), 'pages': page + 1, 'resource_version': version}
        if cursor in cursors:
            raise ValueError(plural + ': repeated continuation')
        cursors.add(cursor)
    raise ValueError(plural + ': page bound reached')


def collect(recorder, kubectl, target):
    record = {'schema': 1, 'claim': 'worker-teardown-only', 'state': 'unavailable',
              'target': target, 'errors': [], 'controls': None, 'inventories': {}}
    try:
        validate(target)
        before = control(recorder, kubectl, target['namespace'], 'before')
        for plural, uid in [('jobs', target['job_uid']), ('pvcs', target['pvc_uid'])]:
            record['inventories'][plural] = inventory(recorder, kubectl, target['namespace'], plural, uid)
        after = control(recorder, kubectl, target['namespace'], 'after')
        if after != before:
            raise ValueError('known-positive connection identities changed')
        record['controls'] = before
        record['state'] = 'absent'
    except (ValueError, KeyError, TypeError, OSError, UnicodeError) as error:
        record['errors'].append(str(error))
    record['readings'] = recorder.rows
    return record


def main():
    request = os.environ.get('OPS_REQUEST_ID', '')
    if len(sys.argv) != 11 or not re.fullmatch(bounded.UUID, request):
        print('worker-teardown: REFUSED fixed target arguments and native ops request required', file=sys.stderr)
        return 78
    fields = ('namespace', 'job_uid', 'pvc_uid', 'run_id', 'acquisition', 'generation', 'pool', 'pool_version', 'amount', 'reclaimer')
    target = dict(zip(fields, sys.argv[1:]))
    try:
        target['generation'] = int(target['generation'])
        target['pool_version'] = int(target['pool_version'])
        validate(target)
    except ValueError as error:
        print('worker-teardown: REFUSED ' + str(error), file=sys.stderr)
        return 78
    work = Path(tempfile.mkdtemp(prefix='boss-worker-teardown-' + request + '-'))
    os.chmod(work, 0o700)
    start = time.monotonic()
    def retain(row):
        with (work / 'readings.jsonl').open('a') as stream:
            stream.write(json.dumps(row, sort_keys=True) + '\n')
    recorder = bounded.Recorder(sink=retain)
    try:
        kubectl = shlex.split(os.environ['WORKER_TEARDOWN_KUBECTL'])
        if not kubectl:
            raise ValueError('existing Kubernetes door unavailable')
        record = collect(recorder, kubectl, target)
        record['request'] = request
        record['observed_at'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        record['source'] = {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in
                            (Path(__file__), Path(__file__).with_suffix('.sh'), Path(bounded.__file__), Path(retention.__file__))}
        receipt = retention.publish(work, request, record, start, prefix='worker-teardown')
        # The full native record is durably attached BEFORE this bounded summary.
        summary = {key: value for key, value in record.items() if key != 'readings'}
        summary.update(manifest_attachment_receipt=receipt, retained_original=str(work))
        print(json.dumps(summary, sort_keys=True))
        return 0 if record['state'] == 'absent' else 1
    except (ValueError, KeyError, TypeError, OSError, UnicodeError) as error:
        print(json.dumps({'schema': 1, 'claim': 'worker-teardown-only', 'state': 'unavailable',
                          'target': target, 'errors': [str(error)], 'retained_original': str(work)}))
        return 1


if __name__ == '__main__':
    sys.exit(main())
