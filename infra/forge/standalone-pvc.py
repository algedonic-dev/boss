"""Read the standalone claim subset used by the reviewed manifest tree.

Do not reserialize YAML: a staged storage edit must preserve unrelated documents.
Unsupported/ambiguous claim declarations refuse, rather than silently disappear.
"""
import argparse
import base64
from decimal import Decimal, DecimalException
import json
from pathlib import Path
import re
import shlex
import subprocess
import uuid
import tempfile
from urllib.parse import quote


def quantity(value):
    match = re.fullmatch(r'(\d+(?:\.\d+)?)([EPTGMK]i|[EPTGMkK]|m|[eE][+-]?\d+)?', str(value))
    if not match:
        raise ValueError('unread storage quantity: ' + str(value))
    suffix = match[2] or ''
    scale = {'': 1, 'm': Decimal('.001'), 'k': 1000, 'K': 1000}
    scale.update({unit: 1000 ** (i + 2) for i, unit in enumerate('MGTPE')})
    scale.update({unit + 'i': 1024 ** (i + 1) for i, unit in enumerate('KMGTPE')})
    try:
        size = Decimal(match[1]) * (Decimal(10) ** int(suffix[1:]) if suffix.startswith(('e', 'E')) and len(suffix) > 1 else scale[suffix])
    except (DecimalException, KeyError) as error:
        raise ValueError('unread storage quantity') from error
    if not size.is_finite() or size <= 0 or size > 2 ** 53 - 1:
        raise ValueError('storage quantity outside positive JSON safe-integer byte range')
    return int(size.to_integral_value(rounding='ROUND_CEILING'))


def _scalar(value):
    value = value.split(' #', 1)[0].strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in '\"\'':
        value = value[1:-1]
    if not re.fullmatch(r'[A-Za-z0-9_.+/-]+', value):
        raise ValueError('unsupported claim scalar: ' + value)
    return value


def _identity(namespace, claim):
    label = r'[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?'
    if (not re.fullmatch(label, namespace) or len(claim) > 253
            or not all(re.fullmatch(label, part) for part in claim.split('.'))):
        raise ValueError('invalid standalone namespace/claim identity')


def documents(directory):
    """One envelope scanner for inventory, namespace-first and isolation.

    Ranges are zero-based, half-open original lines; only inventory parses identity.
    Separators belong to the envelope, not to the object's preserved bytes.
    """
    result = []
    for path in sorted(Path(directory).glob('*.yaml')):
        with path.open(newline='') as source:
            lines = source.read().splitlines(keepends=True)
        start = 0
        separator = ''
        for index in range(len(lines) + 1):
            if index != len(lines) and not re.fullmatch(r'---(?:[ \t]+#[^\r\n]*)?[ \t]*\r?\n?', lines[index]):
                continue
            body = ''.join(lines[start:index])
            kinds = ['kind: ' + _scalar(line.split(':', 1)[1])
                     for line in lines[start:index] if line.startswith('kind:')]
            result.append({'path':path, 'start':start, 'end':index,
                           'body':body, 'separator':separator, 'kinds':kinds})
            if index < len(lines):
                separator = lines[index]
            start = index + 1
    return result


def write_documents(path, rows):
    path.write_text(''.join(('' if i == 0 else row['separator']) + row['body']
                            for i, row in enumerate(rows)))


def namespaces(directory, output):
    """First write contains only Namespace objects, never their mixed files."""
    output.mkdir(exist_ok=True)
    grouped = {}
    for row in documents(directory):
        if 'kind: Namespace' not in row['kinds']:
            continue
        if row['kinds'] != ['kind: Namespace']:
            raise ValueError('ambiguous Namespace document')
        grouped.setdefault(row['path'].name, []).append(row)
    for name, rows in grouped.items():
        write_documents(output / name, rows)


def inventory(directory):
    result = []
    identities = set()
    for document in documents(directory):
        path = document['path']
        lines = path.read_text().splitlines(keepends=True)
        for start, end in [(document['start'], document['end'])]:
            kinds = document['kinds']
            if 'kind: PersistentVolumeClaim' not in kinds:
                continue
            if kinds != ['kind: PersistentVolumeClaim']:
                raise ValueError('ambiguous standalone claim kind in ' + str(path))
            fields = {}
            stack = []
            for index in range(start, end):
                line = lines[index]
                if not line.strip() or line.lstrip().startswith(('#', '---')):
                    continue
                match = re.fullmatch(r'( *)([A-Za-z][A-Za-z0-9_./-]*):(?: *(.*))?\n?', line)
                if not match:
                    raise ValueError('unsupported standalone claim YAML at ' + str(path) + ':' + str(index + 1))
                indent, key, value = len(match[1]), match[2], (match[3] or '').strip()
                # This deliberately small reader cannot interpret YAML blocks.
                # Refuse them before their contents can masquerade as a policy.
                if re.match(r'(?:[!&]\S*\s+)*[|>][1-9+-]*(?:\s|$)', value):
                    raise ValueError('unsupported block scalar in standalone claim')
                scalar = re.sub(r'^(?:[!&]\S*\s+)+', '', value)
                if scalar.startswith(('"', "'")) and not re.fullmatch(
                        r'''(?:"(?:[^"\\]|\\.)*"|'(?:[^']|'')*')(?:\s*#.*)?''', scalar):
                    raise ValueError('unsupported multiline quoted scalar in standalone claim')
                while stack and stack[-1][0] >= indent:
                    stack.pop()
                address = tuple(item[1] for item in stack) + (key,)
                if address in fields:
                    raise ValueError('duplicate claim key in ' + str(path))
                fields[address] = (value, index)
                if not value or value.startswith('#'):
                    stack.append((indent, key))
            def field(*keys):
                if keys not in fields:
                    raise ValueError('missing claim field ' + '.'.join(keys))
                return _scalar(fields[keys][0])
            name, ns = field('metadata', 'name'), field('metadata', 'namespace')
            _identity(ns, name)
            if ('metadata', 'uid') in fields or ('metadata', 'resourceVersion') in fields:
                raise ValueError('runtime UID/resourceVersion must not be declared in source')
            key = ('spec', 'resources', 'requests', 'storage')
            declared = field(*key)
            quantity(declared)
            policy_key = ('metadata', 'annotations', 'boss.algedonic.dev/storage-declaration')
            policy = field(*policy_key) if policy_key in fields else 'exact'
            if policy not in ('exact', 'floor'):
                raise ValueError('unsupported standalone storage declaration policy')
            if (ns, name) in identities:
                raise ValueError('duplicate standalone claim: ' + ns + '/' + name)
            identities.add((ns, name))
            result.append({'namespace': ns, 'claim': name, 'declared': declared,
                           'declared_bytes': quantity(declared), 'storage_policy': policy,
                           'file': 'infra/cluster/manifests/' + path.name,
                           'document_start': start, 'document_end': end,
                           'staged_file': str(path), 'metadata_line': fields[('metadata',)][1] + 1,
                           'line': fields[key][1] + 1})
    return result


def preserve(row, live):
    receipt = dict(row, drift=False, absent=live is None)
    if live is None:
        return receipt
    try:
        meta = live['metadata']
        if live['kind'] != 'PersistentVolumeClaim' or meta['name'] != row['claim'] or meta['namespace'] != row['namespace'] or not meta['uid'] or not isinstance(meta['resourceVersion'], str) or not meta['resourceVersion']:
            raise ValueError('live claim identity does not match declaration')
        requested = live['spec']['resources']['requests']['storage']
        req = quantity(requested)
    except (KeyError, TypeError) as error:
        raise ValueError('live claim is unread/incomplete') from error
    status = live.get('status')
    capacities = status.get('capacity') if isinstance(status, dict) else None
    capacity = capacities.get('storage') if isinstance(capacities, dict) else None
    if capacity is None:
        return dict(receipt, capacity_state='unread', reason='live capacity unavailable',
                    requested=requested, requested_bytes=req, uid=meta['uid'],
                    resource_version=meta['resourceVersion'])
    cap = quantity(capacity)
    receipt.update(live=capacity, live_bytes=cap, requested=requested, requested_bytes=req,
                   uid=meta['uid'], resource_version=meta['resourceVersion'],
                   capacity_state='read',
                   drift=max(req, cap) > row['declared_bytes'] and row.get('storage_policy', 'exact') != 'floor')
    return receipt


def writeback(row, producer, api):
    identity = {'namespace': row['namespace'], 'claim': row['claim']}
    if row.get('affected_claims_complete') is False:
        return {'state':'recording_debt', 'observation':row,
                'reason':'rendered instance roster incomplete; shared-source attribution retained as unknown'}
    # Stable obligation identity across producers. The first observed sizes
    # remain in its title; later observations use conditional metadata.
    packet = str(uuid.uuid5(uuid.UUID('3a2caac3-fc01-4ab9-b420-43b4b5d20844'),
                            json.dumps(identity, sort_keys=True, separators=(',', ':'))))
    body = {'id':packet, 'kind':'backlog-item', 'status':'open',
            'owner_id':'automation:cluster-deploy-runner', 'priority':'standard', 'tags':[],
            'subject':{'subject_kind':'custom', 'id':'bosspipeline'},
            'title':'Tree write-back owed: ' + row['namespace'] + '/' + row['claim']
                    + ' declares ' + row['declared'] + ', live is ' + row['live']
                    + '; shared declaration affects ' + ', '.join(
                        value['namespace'] + '/' + value['claim'] for value in row.get(
                            'affected_claims', [identity])),
            'metadata':{'area':'deploy', 'pvc_tree_writeback':identity}}
    debt = {'state':'recording_debt', 'item':packet, 'observation':row}
    try:
        try:
            code, answer = api('POST', '/api/jobs', body)
        except OSError:
            code, answer = api('POST', '/api/jobs', body)
        if code not in (200, 201, 409) or (code != 409 and answer.get('id') != packet):
            return dict(debt, reason='admission unavailable/refused', http_status=code)
        for _ in range(3):
            code, current = api('GET', '/api/jobs/' + packet)
            if code != 200 or current.get('id') != packet or current.get('kind') != 'backlog-item' or current.get('metadata', {}).get('pvc_tree_writeback') != identity:
                return dict(debt, reason='exact admitted identity unread or different')
            if current.get('status') != 'open':
                return dict(debt, reason='the stable obligation is terminal; no reopen authorized')
            old = current['metadata'].get('pvc_tree_writeback_latest')
            if old is not None and (not isinstance(old, dict) or not isinstance(old.get('observations'), dict)):
                return dict(debt, reason='existing observation schema unread')
            observations = dict(old['observations']) if old else {}
            if set(observations) - {'converge', 'expand'} or producer not in ('converge', 'expand'):
                return dict(debt, reason='unknown writeback producer')
            clean = {key:value for key,value in row.items() if key not in ('staged_file', 'metadata_line')}
            observations[producer] = clean
            patch = {'pvc_tree_writeback_latest':{'observations':observations}}
            expected = {'pvc_tree_writeback':{'present':True, 'value':identity},
                        'pvc_tree_writeback_latest':{'present':old is not None, 'value':old}}
            command = {'contract_version':1, 'expected':expected, 'patch':patch}
            command['idempotency_key'] = str(uuid.uuid5(uuid.UUID(packet), json.dumps(command, sort_keys=True, separators=(',', ':'))))
            try:
                code, answer = api('POST', '/api/jobs/' + packet + '/metadata/conditional', command)
            except OSError:
                actor = getattr(api, 'actor', '')
                code, answer = api('GET', '/api/jobs/' + packet + '/metadata/conditional/' + command['idempotency_key'] + '?actor=' + quote(actor, safe=''))
            if code == 200:
                receipt_job = answer.get('job')
                receipt_metadata = receipt_job.get('metadata') if isinstance(receipt_job, dict) else None
                if (answer.get('contract_version') != 1 or answer.get('job_id') != packet
                        or not getattr(api, 'actor', None) or answer.get('actor') != api.actor
                        or answer.get('command') != command or not answer.get('applied_at')
                        or not answer.get('event_id')
                        or not isinstance(receipt_metadata, dict)
                        or receipt_job.get('id') != packet or receipt_job.get('kind') != 'backlog-item'
                        or receipt_metadata.get('pvc_tree_writeback') != identity
                        or receipt_metadata.get('pvc_tree_writeback_latest') != patch['pvc_tree_writeback_latest']):
                    return dict(debt, reason='conditional response is not the exact actor/command/job receipt')
                return {'state':'recorded', 'item':packet, 'command':command['idempotency_key'],
                        'observation':clean, 'receipt':answer}
            if code != 409:
                return dict(debt, reason='conditional recording unavailable/refused', http_status=code)
        return dict(debt, reason='concurrent observations did not settle within three comparisons')
    except (OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        return dict(debt, reason='writeback not established: ' + str(error))


def stage(directory, kubectl, originals=None, rendered=None):
    receipts = []
    if not kubectl.strip():
        raise ValueError('the existing kubectl adapter is unavailable')
    rows = inventory(directory)
    if originals is not None:
        original_rows = inventory(originals)
        source = {(r['file'], r['namespace'], r['claim']):r for r in original_rows}
        if len(source) != len(original_rows):
            raise ValueError('ambiguous original namespace binding for one source file/claim')
        for row in rows:
            candidates = [value for (file, namespace, claim), value in source.items()
                          if file == row['file'] and claim == row['claim']]
            if len(candidates) > 1:
                raise ValueError('ambiguous original namespace binding for one source file/claim')
            if not candidates:
                raise ValueError('staged claim has no original source declaration')
            original = candidates[0]
            row.update(staged=row['declared'], staged_line=row['line'], line=original['line'],
                       declared=original['declared'], declared_bytes=original['declared_bytes'],
                       storage_policy=original['storage_policy'], source_namespace=original['namespace'])
    roster_refusals = []
    declarations = inventory_all(rendered, roster_refusals) if rendered else rows
    for row in rows:
        matching = [value for value in declarations if
                    (value['file'], value['namespace'], value['claim']) ==
                    (row['file'], row['namespace'], row['claim'])]
        if len(matching) != 1:
            raise ValueError('rendered claim roster missing or ambiguous')
        row['affected_claims'] = matching[0].get('affected_claims', [
            {'namespace':row['namespace'], 'claim':row['claim']}])
        if roster_refusals:
            row.update(affected_claims_complete=False, rendered_roster_refusals=roster_refusals)
    for row in rows:
        command = shlex.split(kubectl) + ['get', 'pvc', row['claim'], '-n', row['namespace'],
                                           '--ignore-not-found=true', '-o', 'json', '--request-timeout=20s']
        output = None
        try:
            output = subprocess.run(command, capture_output=True, text=True, timeout=25)
            if output.returncode:
                receipts.append(dict(row, absent=False, drift=False, capacity_state='unread',
                                     read_exit_code=output.returncode, read_stdout=output.stdout,
                                     read_stderr=output.stderr, reason='live claim read failed; claim skipped'))
                continue
            value = json.loads(output.stdout) if output.stdout.strip() else None
            if output.stdout.strip() and not isinstance(value, dict):
                raise ValueError('live claim read is not one object; not an absence')
            receipts.append(preserve(row, value))
        except (ValueError, KeyError, TypeError, subprocess.TimeoutExpired, OSError) as error:
            unread = dict(row, absent=False, drift=False, capacity_state='unread',
                          reason='live claim unread; claim skipped: ' + str(error))
            if output is not None:
                unread.update(read_exit_code=output.returncode, read_stdout=output.stdout,
                              read_stderr=output.stderr)
            elif isinstance(error, subprocess.TimeoutExpired):
                for key, value in [('stdout', error.stdout), ('stderr', error.stderr)]:
                    if isinstance(value, str):
                        value = value.encode()
                    unread['read_' + key + '_base64'] = base64.b64encode(value or b'').decode('ascii')
                unread['read_timed_out'] = True
            receipts.append(unread)
    return receipts


def create_new(directory, kubectl, mounted, receipts):
    """Create missing claims individually; refusal never aborts other objects."""
    indexed = {(r['namespace'], r['claim']):r for r in receipts if 'namespace' in r and 'claim' in r}
    for path in sorted((Path(directory) / 'standalone-new').glob('*.yaml')):
        matches = [r for r in receipts if 'namespace' in r and 'claim' in r
                   and path.name == r['namespace'] + '--' + r['claim'] + '.yaml']
        if len(matches) != 1 or matches[0].get('apply_state') != 'create_only_absent':
            receipts.append({'apply_state':'held_back_stage_refusal',
                             'create_outcome':{'state':'refused',
                                'reason':'staged claim lacks one verified absent receipt'},
                             'held_back_document':{'audit_file':str(path)}})
            continue
        row = matches[0]
        command = shlex.split(kubectl) + ['create', '--field-manager=kubectl-client-side-apply',
                                        '-f', mounted + '/standalone-new/' + path.name,
                                        '--request-timeout=20s']
        try:
            done = subprocess.run(command, capture_output=True, text=True, timeout=25)
            outcome = {'exit_code':done.returncode, 'stdout':done.stdout, 'stderr':done.stderr,
                       'state':'created' if done.returncode == 0 else 'refused'}
        except (OSError, subprocess.TimeoutExpired) as error:
            outcome = {'state':'unavailable', 'reason':str(error)}
        indexed[(row['namespace'], row['claim'])]['create_outcome'] = outcome
    return receipts


def inventory_all(directory, refusals=None):
    # Observation/expansion need the complete roster and remain strict.
    # Converge alone may isolate an unread instance, retaining explicit
    # incomplete-attribution debt rather than publishing a partial writeback.
    rows = []
    for sub in sorted(Path(directory).iterdir()):
        if not sub.is_dir():
            continue
        try:
            rows.extend(inventory(sub))
        except (ValueError, OSError) as error:
            if refusals is None:
                raise
            refusals.append({'instance':sub.name, 'reason':str(error)})
    keys = [(row['namespace'], row['claim']) for row in rows]
    if len(keys) != len(set(keys)):
        raise ValueError('rendered instances duplicate a standalone claim')
    # The renderer uses one source declaration for every instance. Retain the
    # complete roster so a write-back never implies this line affects only one.
    for row in rows:
        row['affected_claims'] = sorted([
            {'namespace':value['namespace'], 'claim':value['claim']} for value in rows
            if (value['file'], value['line'], value['claim']) ==
               (row['file'], row['line'], row['claim'])], key=lambda value:(value['namespace'],value['claim']))
    return rows


def isolate_claims(directory, receipts):
    """Keep non-PVC documents byte-for-byte, outside the conditional pass."""
    directory = Path(directory)
    by_range = {(r['staged_file'], r['document_start'], r['document_end']):r for r in receipts}
    if len(by_range) != len(receipts):
        raise ValueError('duplicate live-read document receipt')
    for name in ('standalone-existing', 'standalone-new'):
        if (directory / name).exists():
            raise ValueError('standalone staging directory already exists')
    kept = {path:[] for path in sorted(directory.glob('*.yaml'))}
    held = []
    for document in documents(directory):
        path = document['path']
        if 'kind: PersistentVolumeClaim' not in document['kinds']:
            kept[path].append(document)
            continue
        receipt = by_range.pop((str(path), document['start'], document['end']), None)
        if receipt is None or document['kinds'] != ['kind: PersistentVolumeClaim']:
            raise ValueError('staged claim has no matching inventory range')
        target = directory / ('standalone-new' if receipt['absent'] else 'standalone-existing')
        target = target / (receipt['namespace'] + '--' + receipt['claim'] + '.yaml')
        receipt.update(apply_state='create_only_absent' if receipt['absent'] else 'held_back_existing',
                       held_back_document={'file':receipt['file'], 'start':document['start'],
                                           'end':document['end'], 'audit_file':str(target)})
        held.append((target, document['body']))
    if by_range:
        raise ValueError('live-read receipts include a missing staged claim')
    # Validate the whole mapping before modifying any ordinary-apply input.
    for path, body in held:
        path.parent.mkdir(exist_ok=True)
        path.write_text(body)
    for path, rows in kept.items():
        write_documents(path, rows)


def hold_all_claims(directory, reason):
    """Inventory failure is not permission to apply unclassified PVCs."""
    kept = {path:[] for path in sorted(Path(directory).glob('*.yaml'))}
    held = []
    for document in documents(directory):
        path = document['path']
        if 'kind: PersistentVolumeClaim' not in document['kinds']:
            kept[path].append(document)
            continue
        target = Path(directory) / 'standalone-refused' / (path.name + '.' + str(document['start']) + '.yaml')
        held.append((target, document))
    rows = []
    for target, document in held:
        target.parent.mkdir(exist_ok=True)
        target.write_text(document['body'])
        identity = {}
        # Recover identity only through the existing strict inventory reader.
        # Malformed claims remain explicitly unread, never guessed from text.
        try:
            with tempfile.TemporaryDirectory() as scratch:
                (Path(scratch) / document['path'].name).write_text(document['body'])
                known = inventory(Path(scratch))
            if len(known) == 1:
                identity = {key:known[0][key] for key in ('namespace','claim')}
        except (ValueError, KeyError, TypeError, OSError):
            identity = {'identity_state':'unread'}
        rows.append(dict(identity, apply_state='held_back_stage_refusal', absent=False, drift=False,
                     reason=reason, held_back_document={'file':'infra/cluster/manifests/' + document['path'].name,
                     'start':document['start'], 'end':document['end'], 'audit_file':str(target)}))
    for path, kept_rows in kept.items():
        write_documents(path, kept_rows)
    return rows or [{'apply_state':'stage_refusal', 'drift':False, 'reason':reason}]


def enrich(rows, declarations):
    indexed = {(r['namespace'], r['claim']):r for r in declarations}
    result = []
    for row in rows:
        current = dict(row)
        if row.get('pvc_capacity') is not None:
            try:
                current['pvc_capacity_bytes'] = quantity(row['pvc_capacity'])
            except ValueError as error:
                current['pvc_capacity_unread'] = str(error)
        declared = indexed.get((row.get('namespace'), row.get('claim')))
        if declared:
            current['standalone_declaration'] = {
                'namespace':declared['namespace'], 'claim':declared['claim'],
                'bytes':declared['declared_bytes'], 'quantity':declared['declared'],
                'file':declared['file'], 'line':declared['line'],
                'storage_policy':declared['storage_policy']}
        result.append(current)
    return result


def curl_api(base, actor, header):
    """Existing signed jobs door; preserve each raw response before parsing."""
    records = []
    def call(method, path, body=None):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'response'
            args = ['curl', '-sS', '--max-time', '20', '-X', method, '-o', str(output),
                    '-w', '%{http_code}', '-H', 'x-boss-user: ' + json.dumps({'id':actor, 'role':'platform-admin', 'access_tier':'operator'}),
                    '-H', 'Content-Type: application/json']
            if header:
                args += ['-H', header]
            if body is not None:
                payload = Path(directory) / 'body.json'
                payload.write_text(json.dumps(body))
                args += ['--data-binary', '@' + str(payload)]
            timed_out = False
            try:
                done = subprocess.run(args + [base.rstrip('/') + path], capture_output=True, timeout=25)
                code, stdout, stderr = done.returncode, done.stdout, done.stderr
            except subprocess.TimeoutExpired as error:
                timed_out = True
                code, stdout, stderr = None, error.stdout or b'', error.stderr or b''
            raw_bytes = output.read_bytes() if output.exists() else None
            raw = raw_bytes.decode('utf-8', errors='replace') if raw_bytes is not None else None
            stdout_text, stderr_text = stdout.decode('utf-8', errors='replace'), stderr.decode('utf-8', errors='replace')
            records.append({'method':method, 'path':path, 'request':body, 'exit_code':code, 'timed_out':timed_out,
                            'stdout':stdout_text, 'stderr':stderr_text, 'response':raw,
                            'stdout_base64':base64.b64encode(stdout).decode('ascii'),
                            'stderr_base64':base64.b64encode(stderr).decode('ascii'),
                            'response_base64':base64.b64encode(raw_bytes).decode('ascii') if raw_bytes is not None else None})
            if timed_out or code:
                raise OSError('jobs transport failed: ' + ('timeout; ' if timed_out else '') + stderr_text)
            try:
                parsed = json.loads(raw)
            except (ValueError, TypeError):
                parsed = {'unparsed_response':raw}
            return int(stdout_text), parsed if isinstance(parsed, dict) else {'unparsed_json':parsed}
    call.actor = actor
    call.records = records
    return call


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('directory', type=Path)
    parser.add_argument('--stage', metavar='KUBECTL')
    parser.add_argument('--namespaces', type=Path)
    parser.add_argument('--create-new', metavar='KUBECTL')
    parser.add_argument('--mounted')
    parser.add_argument('--receipt', type=Path)
    parser.add_argument('--originals', type=Path)
    parser.add_argument('--rendered', type=Path)
    parser.add_argument('--all', action='store_true')
    parser.add_argument('--enrich', type=Path)
    parser.add_argument('--writebacks', choices=['converge', 'expand'])
    parser.add_argument('--api')
    parser.add_argument('--actor')
    parser.add_argument('--header', default='')
    args = parser.parse_args()
    if args.namespaces is not None:
        namespaces(args.directory, args.namespaces)
        return
    if args.create_new is not None:
        result = create_new(args.directory, args.create_new, args.mounted,
                            json.loads(args.receipt.read_text()))
    elif args.writebacks:
        rows = json.loads(args.directory.read_text())
        api = curl_api(args.api, args.actor, args.header) if args.api and args.actor else None
        result = [writeback(row, args.writebacks, api) if api else
                  {'state':'recording_debt', 'reason':'signed jobs address/actor unavailable', 'observation':row}
                  for row in rows if row.get('drift')]
        if api:
            result = {'outcomes':result, 'transport_records':api.records}
        else:
            result = {'outcomes':result, 'transport_records':[]}
    elif args.enrich:
        result = enrich([json.loads(line) for line in args.enrich.read_text().splitlines()], inventory_all(args.directory))
    elif args.stage is not None:
        try:
            result = stage(args.directory, args.stage, args.originals, args.rendered)
            isolate_claims(args.directory, result)
        except (ValueError, KeyError, TypeError, OSError) as error:
            result = hold_all_claims(args.directory, 'whole stage refused: ' + str(error))
    else:
        result = inventory_all(args.directory) if args.all else inventory(args.directory)
    text = json.dumps(result, sort_keys=True)
    if args.receipt:
        args.receipt.write_text(text + '\n')
    else:
        print(text)


if __name__ == '__main__':
    main()
