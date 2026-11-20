#!/usr/bin/env python3
"""A fixed database target, a reviewed plan, and an explicitly witnessed effect."""
import datetime
import hashlib
import importlib.util
import ipaddress
import json
import os
from pathlib import Path
import re
import shlex
import sys
import tempfile

MIN_AGE_SECONDS = 900
MAX_BACKENDS = 128
MAX_CANDIDATES = 1
# Four complete seven-call snapshots and one signal fit this run. A slow
# native call refuses, rather than borrowing an unmeasured latency assumption.
NATIVE_CALL_SECONDS = 4
MAX_NATIVE_CALLS = 30
UUID = r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}'


class Unproven(ValueError):
    def __init__(self, message, attempted=(), terminated=()):
        super().__init__(message)
        self.attempted, self.terminated = list(attempted), list(terminated)


def instant(value):
    if not isinstance(value, str):
        raise ValueError('native timestamp unread')
    result = datetime.datetime.fromisoformat(value.replace('Z', '+00:00'))
    if result.utcoffset() != datetime.timedelta(0):
        raise ValueError('native timestamp must be UTC')
    return result


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False)


def address_key(value):
    address = ipaddress.ip_address(value)
    if getattr(address, 'scope_id', None):
        raise ValueError('scoped client address is not a native PostgreSQL inet identity')
    return str(getattr(address, 'ipv4_mapped', None) or address)


def plan_bytes(document):
    return (canonical({key: value for key, value in document.items() if key != 'plan_sha256'}) + '\n').encode()


def render(observation):
    if not isinstance(observation, dict):
        raise ValueError('native observation unread')
    now = instant(observation.get('observed_at'))
    binding = observation.get('binding')
    if (not isinstance(binding, dict) or not re.fullmatch(UUID, binding.get('pod_uid', ''))
            or not isinstance(binding.get('database'), str)
            or not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_.-]{0,62}', binding['database'])):
        raise ValueError('database binding unread')
    if instant(binding.get('server_started_at')) > now:
        raise ValueError('database lifetime is in the future')
    pods, backends = observation.get('pods'), observation.get('backends')
    if not isinstance(pods, list) or not isinstance(backends, list) or len(backends) > MAX_BACKENDS:
        raise ValueError('complete Pod/backend census unavailable')
    nodes = observation.get('nodes')
    if not isinstance(nodes, list) or not nodes:
        raise ValueError('complete node address and Pod CIDR census unavailable')
    ips, pod_ids = set(), set()
    for pod in pods:
        if (not isinstance(pod, dict) or not re.fullmatch(UUID, pod.get('uid', ''))
                or pod['uid'] in pod_ids or not isinstance(pod.get('ips'), list)):
            raise ValueError('Pod lifetime or duplicate census unread')
        pod_ids.add(pod['uid'])
        addresses = sorted({address_key(value) for value in pod['ips']})
        ips.update(addresses)
    if binding['pod_uid'] not in pod_ids:
        raise ValueError('bound database Pod absent from complete live census')
    node_ids, networks = set(), []
    for node in nodes:
        if (not isinstance(node, dict) or not re.fullmatch(UUID, node.get('uid', ''))
                or node['uid'] in node_ids or not isinstance(node.get('ips'), list)
                or not node['ips'] or not isinstance(node.get('pod_cidrs'), list) or not node['pod_cidrs']):
            raise ValueError('node address or Pod CIDR census unread')
        node_ids.add(node['uid'])
        ips.update(address_key(value) for value in node['ips'])
        networks.extend(ipaddress.ip_network(value, strict=True) for value in node['pod_cidrs'])
    candidates, pids = [], set()
    for backend in backends:
        if not isinstance(backend, dict) or type(backend.get('pid')) is not int or backend['pid'] <= 0 or backend['pid'] in pids:
            raise ValueError('backend PID missing or duplicated')
        pids.add(backend['pid'])
        if backend.get('backend_type') != 'client backend' or backend.get('state') != 'active' or backend.get('database') != binding['database']:
            continue
        if backend.get('client_addr') is None:
            continue  # Local administrative connections are never candidates.
        address = address_key(backend['client_addr'])
        parsed = ipaddress.ip_address(address)
        if (parsed.is_loopback or parsed.is_link_local or parsed.is_multicast or parsed.is_unspecified
                or not any(parsed.version == network.version and parsed in network for network in networks)):
            continue  # Unknown host/external clients are not proven dead Pods.
        start, query = instant(backend.get('backend_start')), instant(backend.get('query_start'))
        if start > query or query > now or type(backend.get('client_port')) is not int or not 0 < backend['client_port'] <= 65535:
            raise ValueError('backend native identity unread')
        if address in ips or (now - query).total_seconds() < MIN_AGE_SECONDS:
            continue
        candidates.append({key: backend[key] for key in (
            'pid', 'backend_start', 'query_start', 'client_addr', 'client_port', 'backend_type', 'state', 'database')})
    if not candidates:
        raise ValueError('no qualifying old orphaned client backend')
    ordered = sorted(candidates, key=lambda row: (instant(row['query_start']), instant(row['backend_start']), row['pid']))
    document = {'schema': 2, 'scope': 'fixed serving database; cluster Pod CIDR client with no live Pod or node owner',
                'minimum_query_age_seconds': MIN_AGE_SECONDS, 'binding': binding,
                'ownership': 'complete cluster-wide live Pod and node census; candidate addresses unowned',
                'eligible_count': len(ordered), 'selection': 'oldest query, then backend lifetime, then PID; one target per approval',
                'candidates': ordered[:MAX_CANDIDATES], 'deferred': ordered[MAX_CANDIDATES:],
                'race_limit': 'Pod census and PostgreSQL signaling are separate resources, not a cross-resource lock'}
    return {**document, 'plan_sha256': hashlib.sha256(plan_bytes(document)).hexdigest()}


def plan(native):
    return render(native.snapshot())


def execute(native, approved_hash):
    if not isinstance(approved_hash, str) or not re.fullmatch(r'[0-9a-f]{64}', approved_hash):
        raise ValueError('approved plan hash required; nothing was terminated')
    original = plan(native)
    if original['plan_sha256'] != approved_hash or plan(native) != original:
        raise ValueError('approved plan changed; nothing was terminated')
    attempted, terminated = [], []
    try:
        for backend in original['candidates']:
            fresh = render(native.snapshot())
            if fresh != original:
                raise ValueError('database, Pod census or candidate identity changed before signal')
            # Any lost acknowledgment after this point is an unknown effect.
            attempted.append(backend['pid'])
            if native.terminate(original['binding'], backend) is not True:
                raise ValueError('termination signal refused or unacknowledged')
            observed = native.snapshot()
            if observed.get('binding') != original['binding']:
                raise ValueError('database lifetime changed after signal')
            rows = observed.get('backends')
            if (not isinstance(rows, list) or len(rows) > MAX_BACKENDS
                    or any(not isinstance(row, dict) or type(row.get('pid')) is not int for row in rows)
                    or len({row['pid'] for row in rows}) != len(rows)):
                raise ValueError('post-signal backend census unread')
            if any(row.get('pid') == backend['pid'] and row.get('backend_start') == backend['backend_start'] for row in rows):
                raise ValueError('original client backend is still present')
            terminated.append(backend['pid'])
    except (ValueError, KeyError, TypeError, OSError) as error:
        if attempted:
            raise Unproven('termination requested; effect NOT proven: ' + str(error), attempted, terminated) from error
        raise
    return {'state': 'proven', 'plan_sha256': approved_hash, 'attempted': attempted, 'terminated': terminated,
            'deferred': original['deferred'], 'eligible_count_at_approval': original['eligible_count'],
            'claim': 'only the planned original client sessions are absent; no stall-cause or latency claim'}


def load_database_module():
    spec = importlib.util.spec_from_file_location('fixed_database_observation', Path(__file__).with_name('jobs-database.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Native:
    def __init__(self, recorder, kubectl, database_module):
        self.recorder, self.kubectl, self.db = recorder, kubectl, database_module

    def read(self, label, argv):
        return self.db.bounded.text(self.recorder.call(label, argv))

    def get(self, kind, name):
        return self.db.bounded.one_json(self.read('get.' + kind, [*self.kubectl, '-n', 'boss', '--request-timeout=15s', 'get', kind, name, '-o', 'json']))

    def pods(self):
        # kubectl completes the cluster-wide list (including server pagination)
        # before rendering this fixed projection. Avoid transporting every
        # Pod's embedded workflow/managedFields to establish an IP roster.
        # kubectl's aggregated List does not expose resourceVersion (observed
        # with both chunk-size 1 and 0). Do not invent an RV or claim an atomic
        # Pod/IP lease; retain the complete successful CLI projection instead.
        projection = ('jsonpath={range .items[*]}{.metadata.namespace}{"\\t"}{.metadata.name}{"\\t"}'
                      '{.metadata.uid}{"\\t"}{.status.phase}{"\\t"}{.status.podIPs[*].ip}{"\\n"}{end}')
        raw = self.read('pods.complete', [*self.kubectl, '--request-timeout=15s',
                                       'get', 'pods', '--all-namespaces', '--chunk-size=32', '-o', projection])
        lines = raw.splitlines()
        if not lines or len(lines) > 1024:
            raise ValueError('complete Pod inventory empty or bound exceeded')
        result, all_uids, postgres_seen = [], set(), False
        for line in lines:
            fields = line.split('\t')
            if len(fields) != 5:
                raise ValueError('Pod projection field missing')
            namespace, name, uid, phase, addresses = fields
            if not re.fullmatch(r'[a-z0-9][a-z0-9-]*', namespace) or not re.fullmatch(UUID, uid) or uid in all_uids:
                raise ValueError('Pod inventory identity unread or duplicate')
            all_uids.add(uid)
            postgres_seen = postgres_seen or (namespace == 'boss' and name == 'postgres-0')
            if phase in ('Succeeded', 'Failed'):
                continue
            if phase not in ('Pending', 'Running'):
                raise ValueError('Pod lifecycle unread')
            ips = addresses.split()
            if phase == 'Running' and not ips:
                raise ValueError('Pod addresses unread')
            result.append({'uid': uid, 'ips': ips})
        if not postgres_seen:
            raise ValueError('positive postgres Pod missing from complete census')
        return result

    def nodes(self):
        projection = ('jsonpath={range .items[*]}{.metadata.name}{"\\t"}{.metadata.uid}{"\\t"}'
                      '{.status.addresses[?(@.type=="InternalIP")].address}{" "}'
                      '{.status.addresses[?(@.type=="ExternalIP")].address}{"\\t"}'
                      '{.spec.podCIDRs[*]}{"\\n"}{end}')
        raw = self.read('nodes.complete', [*self.kubectl, '--request-timeout=15s', 'get', 'nodes', '--chunk-size=32', '-o', projection])
        lines = raw.splitlines()
        if not lines or len(lines) > 1024:
            raise ValueError('complete node inventory empty or bound exceeded')
        rows = []
        for line in lines:
            fields = line.split('\t')
            if len(fields) != 4:
                raise ValueError('node projection field missing')
            _, uid, addresses, cidrs = fields
            rows.append({'uid': uid, 'ips': addresses.split(), 'pod_cidrs': cidrs.split()})
        return rows

    def sql(self, database, query, mutate=False):
        argv = [*self.kubectl, '-n', 'boss', '--request-timeout=15s', 'exec', 'postgres-0', '-c', 'postgres', '--',
                'timeout', '12', 'psql', '-X', '-q', '-At', '-v', 'ON_ERROR_STOP=1', '-U', 'boss', '-d', database,
                '-c', "SET timezone='UTC'", '-c', 'SET statement_timeout=8000', '-c', 'SET lock_timeout=1000']
        if not mutate:
            argv += ['-c', 'SET default_transaction_read_only=on']
        argv += ['-c', query]
        return self.db.bounded.one_json(self.read('sql.terminate' if mutate else 'sql.census', argv))

    def snapshot(self):
        process = self.db.bounded.one_json(self.read('serving.target', ['bash', str(Path(__file__).with_name('read-jobs-database.sh')), '--read-target']))
        target = self.db.declared_target(process.get('target'))
        pod = self.get('pod', 'postgres-0')
        meta, status = pod.get('metadata', {}), pod.get('status', {})
        if (meta.get('namespace'), meta.get('name'), status.get('phase')) != ('boss', 'postgres-0', 'Running') or meta.get('deletionTimestamp'):
            raise ValueError('fixed postgres Pod is not running')
        uid = meta.get('uid')
        if not isinstance(uid, str) or not re.fullmatch(UUID, uid):
            raise ValueError('fixed postgres Pod UID unread')
        bound = self.db.bind_target(target, self.get('service', 'postgres'), self.get('endpoints', 'postgres'), {'pod_uid': uid})
        live, nodes = self.pods(), self.nodes()
        value = self.sql(target['database'], CENSUS_SQL)
        if (value.get('database') != target['database'] or value.get('read_only') != 'on'
                or type(value.get('count')) is not int or not 0 <= value['count'] <= MAX_BACKENDS
                or value['count'] != len(value.get('backends', []))):
            raise ValueError('complete fixed database census unavailable')
        return {'observed_at': value['observed_at'], 'binding': {**bound, 'server_started_at': value['server_started_at']},
                'pods': live, 'nodes': nodes, 'backends': value['backends']}

    def terminate(self, binding, backend):
        # Only native-validated scalar identity fields enter this fixed SQL.
        stamp = lambda value: "'" + instant(value).isoformat() + "'::timestamptz"
        address_key(backend['client_addr'])
        address = str(ipaddress.ip_address(backend['client_addr']))
        query = ("SELECT jsonb_build_object('matches',count(*),'terminated',bool_and(pg_terminate_backend(pid,1000))) "
                 "FROM pg_stat_activity WHERE pid=" + str(backend['pid']) + " AND pid<>pg_backend_pid() AND backend_type='client backend' AND state='active'"
                 " AND datname=current_database() AND backend_start=" + stamp(backend['backend_start']) + " AND query_start=" + stamp(backend['query_start'])
                 + " AND client_addr='" + address + "'::inet AND client_port=" + str(backend['client_port'])
                 + " AND query_start<=clock_timestamp()-interval '" + str(MIN_AGE_SECONDS) + " seconds' AND pg_postmaster_start_time()=" + stamp(binding['server_started_at']))
        result = self.sql(binding['database'], query, mutate=True)
        return result.get('matches') == 1 and result.get('terminated') is True


CENSUS_SQL = """WITH clients AS MATERIALIZED (
 SELECT pid, backend_start, query_start, client_addr, client_port, backend_type,state,datname AS database
 FROM pg_stat_activity WHERE backend_type='client backend' AND datname=current_database() AND pid<>pg_backend_pid()
) SELECT jsonb_build_object('database',current_database(),'read_only',current_setting('transaction_read_only'),
 'observed_at',clock_timestamp(),'server_started_at',pg_postmaster_start_time(),'count',(SELECT count(*) FROM clients),
 'backends',(SELECT COALESCE(jsonb_agg(to_jsonb(c) ORDER BY pid),'[]'::jsonb) FROM (SELECT * FROM clients ORDER BY pid LIMIT 129)c))"""


def main():
    if len(sys.argv) != 2 or (sys.argv[1] != '--plan' and not re.fullmatch(r'[0-9a-f]{64}', sys.argv[1])):
        print('orphan-backends: REFUSED — fixed plan or approved hash required', file=sys.stderr)
        return 78
    request = os.environ.get('OPS_REQUEST_ID', '')
    if not re.fullmatch(UUID, request):
        print('orphan-backends: REFUSED — native request identity required', file=sys.stderr)
        return 78
    work = Path(tempfile.mkdtemp(prefix='boss-orphan-backends-' + request + '-'))
    os.chmod(work, 0o700)
    db = load_database_module()
    def retain(row):
        with (work / 'native.jsonl').open('a') as stream:
            stream.write(canonical(row) + '\n')
    def native_command(argv, timeout):
        return db.bounded.native_command(argv, min(timeout, NATIVE_CALL_SECONDS))
    recorder = db.bounded.Recorder(execute=native_command, limit=MAX_NATIVE_CALLS, sink=retain)
    native = Native(recorder, shlex.split(os.environ['ORPHAN_BACKENDS_KUBECTL']), db)
    outcome, code = None, 1
    try:
        outcome = plan(native) if sys.argv[1] == '--plan' else execute(native, sys.argv[1])
        code = 0
    except (ValueError, KeyError, TypeError, OSError) as error:
        outcome = {'state': 'unproven' if isinstance(error, Unproven) else 'refused', 'error': str(error),
                   'attempted': getattr(error, 'attempted', []), 'terminated': getattr(error, 'terminated', [])}
    # This is the runner's durable stdout/stderr even if upload refuses. A
    # successful readback is provisional until its original evidence lands.
    reported = {**outcome, 'state': 'effect-observed-evidence-pending'} if code == 0 and sys.argv[1] != '--plan' else outcome
    print(canonical({'outcome': reported}), file=sys.stderr if sys.argv[1] == '--plan' else sys.stdout, flush=True)
    path = work / 'result.json'
    path.write_text(canonical({'outcome': outcome, 'native_receipts': recorder.rows}) + '\n')
    if path.stat().st_size > 2_096_128:
        print('orphan-backends: evidence retained; attachment bound exceeded: ' + str(work), file=sys.stderr)
        return 1
    try:
        # Evidence publication has its own one-call budget; observation
        # exhaustion must not consume the last chance to retain its effect.
        publisher = db.bounded.Recorder(limit=1, sink=retain)
        attachment = db.bounded.text(publisher.call('attach-evidence', ['boss', 'attach', request, str(path)]))
    except (ValueError, OSError) as error:
        print(canonical({'state': 'unproven', 'error': 'evidence attachment unproven: ' + str(error),
                         'attempted': outcome.get('attempted', []), 'terminated': outcome.get('terminated', []),
                         'retained_original': str(work)}), file=sys.stderr, flush=True)
        return 1
    # The runner hashes stdout verbatim. Native timestamps, attachment receipts
    # and local paths must never change the plan David's passkey signs.
    print(canonical({'attachment': attachment, 'retained_original': str(work)}), file=sys.stderr)
    if code == 0 and sys.argv[1] == '--plan':
        sys.stdout.buffer.write(plan_bytes(outcome))
        print('plan-sha256: ' + outcome['plan_sha256'], file=sys.stderr)
    return code


if __name__ == '__main__':
    sys.exit(main())
