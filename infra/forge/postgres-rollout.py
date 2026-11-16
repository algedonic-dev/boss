"""Converge PostgreSQL through a local, UID-fenced last-good receipt.

The jobs API is deliberately absent from this recovery loop. Every command's
complete output is retained locally for the existing converge packet recorder.
"""
import argparse
import copy
from datetime import datetime, timezone
import fcntl
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import tarfile
import tempfile
import time

# The rollback target a bootstrap forward journals: no live StatefulSet, so
# nothing to restore. resume_bootstrap reads it as nothing to undo (bd5964f2).
BOOTSTRAP_TARGET = {'state': 'not-applicable-bootstrap'}

# The wall clock each leg of rendering a named source gets: the git archive
# of infra/cluster and examples (about 2 MiB, 106 files on 2026-11-06) and
# the renderer over it. A decided value, kept at 60 s (backlog aa0013a5):
# a recovery waits on it before it may restore anything, and a forge that
# cannot read 2 MiB in a minute is a fault to report in words, not to wait
# out. A test that
# proves what a commit renders lifts it on its instance
# (named_source_seconds = None); it is not a gate bay's budget.
NAMED_SOURCE_SECONDS = 60

# How long a stale failed-revision postgres-0 may sit in ContainerCreating,
# counted from when it was scheduled, before the rollback's recycle reads it
# as failure evidence (backlog 76f89301). It counts only when no container
# in the pod has started (nothing_started below), so recycling it interrupts
# nothing; the bound only refuses to call a pod that was scheduled a moment
# ago stuck. It equals the rollout
# wait the rollback has already spent, so a pod still creating then was
# never going to come up on its own.
CONTAINER_CREATING_SECONDS = 300


def tolerates(toleration, taint):
    """One toleration against one taint, by the scheduler's matching rule."""
    if toleration.get('effect') and toleration['effect'] != taint.get('effect'):
        return False
    if toleration.get('operator', 'Equal') == 'Exists':
        return not toleration.get('key') or toleration['key'] == taint.get('key')
    return toleration.get('key') == taint.get('key') and toleration.get('value', '') == taint.get('value', '')


def selector_requirement(requirement, values_by_key):
    key, operator, values = requirement.get('key'), requirement.get('operator'), requirement.get('values') or []
    present, value = key in values_by_key, values_by_key.get(key)
    if operator == 'In':
        return present and value in values
    if operator == 'NotIn':
        return not present or value not in values
    if operator == 'Exists':
        return present
    if operator == 'DoesNotExist':
        return not present
    if operator in ('Gt', 'Lt') and len(values) == 1 and re.fullmatch(r'-?[0-9]+', values[0]):
        if not present or not re.fullmatch(r'-?[0-9]+', value or ''):
            return False
        return int(value) > int(values[0]) if operator == 'Gt' else int(value) < int(values[0])
    raise ValueError('unread PostgreSQL placement requirement: ' + json.dumps(requirement))


def unfit(node, pod):
    """Why one node cannot take the pod template, or None when it can.

    The scheduler's hard filters that a Node object alone decides: Ready,
    not cordoned, every NoSchedule/NoExecute taint tolerated, the
    nodeSelector, and the required node affinity (backlog 76f89301).
    Resources and volume topology stay the scheduler's to judge.
    """
    metadata, spec = node.get('metadata') or {}, node.get('spec') or {}
    name, labels = metadata.get('name'), metadata.get('labels') or {}
    if not any(c.get('type') == 'Ready' and c.get('status') == 'True'
               for c in (node.get('status') or {}).get('conditions') or []):
        return 'not Ready'
    if spec.get('unschedulable'):
        return 'cordoned'
    for taint in spec.get('taints') or []:
        if (taint.get('effect') in ('NoSchedule', 'NoExecute')
                and not any(tolerates(t, taint) for t in pod.get('tolerations') or [])):
            return 'untolerated taint %s=%s:%s' % (taint.get('key'), taint.get('value', ''), taint['effect'])
    for key, value in sorted((pod.get('nodeSelector') or {}).items()):
        if labels.get(key) != value:
            return 'nodeSelector %s=%s unmatched' % (key, value)
    required = ((pod.get('affinity') or {}).get('nodeAffinity') or {}).get('requiredDuringSchedulingIgnoredDuringExecution')
    if required is not None:
        # Terms are ORed, requirements within a term ANDed, and an empty
        # term matches nothing, as the scheduler reads them.
        def term(row):
            expressions, fields = row.get('matchExpressions') or [], row.get('matchFields') or []
            if any(field.get('key') != 'metadata.name' for field in fields):
                raise ValueError('unread PostgreSQL placement field: ' + json.dumps(fields))
            # Evaluate every requirement, so an unread one refuses whichever node is read first.
            results = ([selector_requirement(e, labels) for e in expressions]
                       + [selector_requirement(f, {'metadata.name': name}) for f in fields])
            return bool(results) and all(results)
        if not any([term(row) for row in required.get('nodeSelectorTerms') or []]):
            return 'required node affinity unmatched'
    return None


def scheduled_seconds(pod, now):
    """Seconds since the pod was bound to a node, or None when unread."""
    for condition in (pod.get('status') or {}).get('conditions') or []:
        if condition.get('type') == 'PodScheduled' and condition.get('status') == 'True':
            try:
                then = datetime.strptime(condition.get('lastTransitionTime') or '', '%Y-%m-%dT%H:%M:%SZ')
            except ValueError:
                return None
            return (now - then.replace(tzinfo=timezone.utc)).total_seconds()
    return None


def nothing_started(statuses):
    """True only when every container status (ordinary, init, ephemeral)
    shows a container that has never started: waiting now, never else.

    Review cc5a878a B1 on car 1d5379b5: container state is per-container
    (https://kubernetes.io/docs/concepts/workloads/pods/pod-lifecycle/#container-states),
    so one sidecar aged in ContainerCreating recycled a pod whose postgres
    was running with no restart. The Unschedulable and ContainerCreating
    reasons now need this positive fact; a started container of any name
    may be postgres or may hold its volume, so either refuses the recycle,
    and the stuck volume attach those reasons exist for starts nothing.
    An unscheduled pod has no statuses, and nothing has started in it.
    """
    for c in statuses:
        if (set(c.get('state') or {}) != {'waiting'} or c.get('started') is True
                or any(k in (c.get('lastState') or {}) for k in ('running', 'terminated'))):
            return False
    return True


def failure_evidence(pod, now):
    """What makes a stale failed-revision pod a failure, or None.

    A restart or a crash/error/pull reason was the only evidence until
    76f89301. A pin no node can take leaves postgres-0 Pending with no
    container status at all, and a volume that never attaches leaves it
    in ContainerCreating; both read as no evidence, so the recycle
    refused and the system of record stayed dark until a hand deleted
    the pod. Those two reasons count only when nothing_started proves
    postgres is not running, so neither recycle can interrupt a WAL
    recovery.
    """
    status = pod.get('status') or {}
    statuses = (status.get('containerStatuses') or []) + (status.get('initContainerStatuses') or [])
    for c in statuses:
        if c.get('restartCount', 0) > 0:
            return 'restarted %s times' % c['restartCount']
        for state in ('waiting', 'terminated'):
            reason = (c.get('state') or {}).get(state, {}).get('reason')
            if reason in ('CrashLoopBackOff', 'Error', 'ImagePullBackOff'):
                return reason
    if not nothing_started(statuses + (status.get('ephemeralContainerStatuses') or [])):
        return None
    for condition in status.get('conditions') or []:
        if (condition.get('type') == 'PodScheduled' and condition.get('status') == 'False'
                and condition.get('reason') == 'Unschedulable'
                and not (pod.get('spec') or {}).get('nodeName')):
            return 'Unschedulable'
    # Postgres must itself be seen waiting: a status list that omits it
    # proves nothing about it (review cc5a878a B1).
    if not any(c.get('name') == 'postgres' for c in status.get('containerStatuses') or []):
        return None
    creating = [c for c in statuses if (c.get('state') or {}).get('waiting', {}).get('reason') == 'ContainerCreating']
    if creating:
        elapsed = scheduled_seconds(pod, now)
        if elapsed is not None and elapsed >= CONTAINER_CREATING_SECONDS:
            return 'ContainerCreating %d s after scheduling (bound %d s)' % (elapsed, CONTAINER_CREATING_SECONDS)
    return None


def atomic(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix('.tmp')
    with temporary.open('w') as output:
        output.write(json.dumps(value, indent=2) + '\n')
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def changes(before, after, path='/spec'):
    if isinstance(before, dict) and isinstance(after, dict):
        result = []
        for key in sorted(before.keys() | after.keys()):
            child = path + '/' + key.replace('~', '~0').replace('/', '~1')
            if key in before and key in after:
                result.extend(changes(before[key], after[key], child))
            else:
                row = {'path': child, 'before_present': key in before, 'after_present': key in after}
                if key in before:
                    row['before'] = before[key]
                if key in after:
                    row['after'] = after[key]
                result.append(row)
        return result
    # Kubernetes computes list merge keys. The inverse treats its resulting
    # arrays atomically, refusing an intervening array edit rather than guessing.
    return [] if before == after else [{'path': path, 'before_present': True,
                                       'after_present': True, 'before': before, 'after': after}]


def value_at(obj, path):
    for key in path.lstrip('/').split('/'):
        key = key.replace('~1', '/').replace('~0', '~')
        if not isinstance(obj, dict) or key not in obj:
            return False, None
        obj = obj[key]
    return True, obj


def receipt_stdout(raw):
    """Omit Kubernetes bookkeeping, retaining the full diagnostic elsewhere."""
    try:
        value = json.loads(raw)
    except (ValueError, TypeError):
        return raw, False
    changed = False
    def clean(obj):
        nonlocal changed
        if isinstance(obj, dict):
            metadata = obj.get('metadata')
            if isinstance(metadata, dict) and 'managedFields' in metadata:
                del metadata['managedFields']
                changed = True
            for child in obj.values():
                clean(child)
        elif isinstance(obj, list):
            for child in obj:
                clean(child)
    clean(value)
    return (json.dumps(value) if changed else raw), changed


def journal_digest(attempt):
    # State and failure observations may change; the inverse's identity cannot.
    try:
        identity = {key: attempt[key] for key in ('source', 'namespace', 'name', 'forward_uid',
                                                 'rollback_target', 'forward_inverse')}
    except (KeyError, TypeError) as error:
        raise ValueError('PostgreSQL active journal digest fields are unread') from error
    return hashlib.sha256(json.dumps(identity, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


class Recovery:
    def __init__(self, args):
        self.args = args
        self.read = shlex.split(args.read)
        self.write = shlex.split(args.write)
        self.record = {'namespace': args.namespace, 'source': args.head, 'commands': [],
                       'outcome': 'not-completed', 'postgres_present': None, 'postgres_failure': None}
        self.active_reference = None
        self.named_source_seconds = NAMED_SOURCE_SECONDS

    def run(self, argv, body=None, check=True, timeout=480, env=None):
        try:
            result = subprocess.run(argv, input=body, capture_output=True, text=True, timeout=timeout, env=env)
            event = {'argv': argv, 'exit_code': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr}
        except subprocess.TimeoutExpired as error:
            def text(value):
                return value.decode(errors='replace') if isinstance(value, bytes) else value or ''
            event = {'argv': argv, 'exit_code': 124, 'stdout': text(error.stdout), 'stderr': text(error.stderr), 'timed_out': True}
        stored = copy.deepcopy(event)
        stored['stdout'], omitted = receipt_stdout(event['stdout'])
        if omitted:
            stored['stdout_omitted'] = 'Kubernetes metadata.managedFields only'
            stored['stdout_original_bytes'] = len(event['stdout'].encode())
            stored['stdout_original_sha256'] = hashlib.sha256(event['stdout'].encode()).hexdigest()
        self.record['commands'].append(stored)
        atomic(Path(self.args.receipt), self.record)
        if check and event['exit_code']:
            raise ValueError('command failed: ' + json.dumps(stored))
        return event

    def kubectl(self, argv, body=None, check=True, timeout=480):
        return self.run((self.write if body is not None else self.read) + argv, body, check, timeout=timeout)

    def live(self):
        event = self.kubectl(['get', 'sts/postgres', '-n', self.args.namespace, '--ignore-not-found', '-o', 'json', '--request-timeout=30s'])
        if not event['stdout'].strip():
            return None
        obj = json.loads(event['stdout'])
        metadata = obj.get('metadata', {})
        if (obj.get('kind') != 'StatefulSet' or metadata.get('name') != 'postgres'
                or metadata.get('namespace') != self.args.namespace or not metadata.get('uid')
                or not metadata.get('resourceVersion') or not isinstance(obj.get('spec'), dict)):
            raise ValueError('unread PostgreSQL StatefulSet identity/spec')
        return obj

    def ready(self, obj):
        status, metadata = obj.get('status', {}), obj['metadata']
        replicas = obj['spec'].get('replicas', 1)
        return (status.get('observedGeneration', 0) >= metadata.get('generation', 1)
                and status.get('readyReplicas', 0) == replicas
                and status.get('updatedReplicas', 0) == replicas
                and status.get('currentRevision') is not None
                and status.get('currentRevision') == status.get('updateRevision'))

    def wait(self, expected_uid):
        self.kubectl(['rollout', 'status', 'sts/postgres', '-n', self.args.namespace, '--timeout=300s', '--request-timeout=330s'])
        obj = self.live()
        if obj is not None and obj['metadata']['uid'] != expected_uid:
            raise ValueError('PostgreSQL StatefulSet identity changed during readiness wait')
        if obj is None or not self.ready(obj):
            raise ValueError('PostgreSQL rollout returned without observed ready current generation')
        return obj

    def fence(self, expected):
        obj = self.live()
        if obj is None or obj['metadata']['uid'] != expected['metadata']['uid']:
            raise ValueError('PostgreSQL StatefulSet identity changed')
        return obj

    def merged_spec(self, obj, spec):
        # Compare in the server's defaulted form: volumeClaimTemplates is an
        # atomic list, so a local merge drops defaults from every source PVC.
        # Dry-run preserves omitted foreign fields without committing a write;
        # the real JSON Patch below still fences the observed UID and version.
        identity = {key: obj['metadata'][key] for key in ('uid', 'resourceVersion')}
        body = json.dumps({'metadata': identity, 'spec': spec})
        merged = json.loads(self.kubectl(['patch', 'sts/postgres', '-n', self.args.namespace,
                                        '--type=strategic', '-p', body, '--dry-run=server',
                                        '-o', 'json', '--request-timeout=30s'], body, timeout=35)['stdout'])
        expected = dict(identity, name='postgres', namespace=self.args.namespace)
        if (merged.get('kind') != obj['kind']
                or any(merged.get('metadata', {}).get(key) != value for key, value in expected.items())
                or not isinstance(merged.get('spec'), dict)):
            raise ValueError('server PostgreSQL strategic merge changed identity or returned unread spec')
        return merged['spec']

    def patch(self, expected, spec):
        obj = self.fence(expected)
        good = self.record.get('rollback_target', {})
        if not isinstance(good.get('spec'), dict) or self.merged_spec(obj, good['spec']) != obj['spec']:
            raise ValueError('PostgreSQL spec moved independently from the namespace ready target; forward refused')
        merged = self.merged_spec(obj, spec)
        self.record['forward_inverse'] = {'uid': obj['metadata']['uid'], 'source': good['source'],
                                          'changes': changes(obj['spec'], merged)}
        if not self.record['forward_inverse']['changes']:
            return
        # Before anything is journaled: a refused placement writes nothing,
        # so this head is not quarantined and may try again once a node can
        # take the pod (76f89301).
        self.placement(merged)
        # Journal the precise inverse before a lost ACK or process exit can
        # hide which additions must be removed (not merely merged away).
        atomic(Path(self.args.receipt), self.record)
        operations = [{'op': 'test', 'path': '/metadata/uid', 'value': obj['metadata']['uid']},
                      {'op': 'test', 'path': '/metadata/resourceVersion', 'value': obj['metadata']['resourceVersion']},
                      {'op': 'replace', 'path': '/spec', 'value': merged}]
        self.journal_forward(obj['metadata']['uid'])
        self.kubectl(['patch', 'sts/postgres', '-n', self.args.namespace, '--type=json', '-p', json.dumps(operations), '--request-timeout=60s'])

    def placement(self, spec):
        """Refuse a forward whose pod template no Ready node can take.

        Review dd6733cc F2 on car 526974b0 (backlog 76f89301): the forward
        patch replaces postgres-0, and a pin nothing can satisfy leaves the
        system of record Pending. The refusal names every node and why.
        """
        template = spec.get('template') or {}
        if not isinstance(template, dict) or not isinstance(template.get('spec') or {}, dict):
            raise ValueError('unread PostgreSQL pod template; placement refused')
        pod = template.get('spec') or {}
        nodes = json.loads(self.kubectl(['get', 'nodes', '-o', 'json', '--request-timeout=30s'])['stdout'])
        if not isinstance(nodes, dict) or not isinstance(nodes.get('items'), list):
            raise ValueError('unread node list; PostgreSQL placement refused')
        reasons = {}
        for row in nodes['items']:
            name = (row.get('metadata') or {}).get('name')
            if not isinstance(name, str) or not name:
                raise ValueError('unread node identity; PostgreSQL placement refused')
            reasons[name] = unfit(row, pod)
        fits = sorted(name for name, reason in reasons.items() if reason is None)
        self.record['placement'] = {'fits': fits, 'refused': {k: v for k, v in sorted(reasons.items()) if v}}
        atomic(Path(self.args.receipt), self.record)
        if not fits:
            raise ValueError('PostgreSQL pin fits no Ready node; forward refused before any write: '
                             + '; '.join('%s: %s' % row for row in sorted(reasons.items())))

    def journal_forward(self, uid):
        # This record survives the per-run receipt and precedes even a lost ACK.
        attempt = {'source': self.args.head, 'namespace': self.args.namespace,
                   'name': 'postgres', 'uid': uid, 'forward_uid': uid, 'state': 'attempting',
                   'rollback_target': self.record['rollback_target'],
                   'forward_inverse': self.record.get('forward_inverse')}
        if self.active_path.exists():
            raise ValueError('PostgreSQL active journal is already present; forward refused')
        atomic(self.quarantine, attempt)
        reference = {key: attempt[key] for key in ('source', 'namespace', 'name')}
        reference.update(uid=uid, inverse_digest=journal_digest(attempt))
        atomic(self.active_path, reference)
        self.active_reference = reference
        atomic(Path(self.args.receipt), self.record)
        self.record['forward_issued'] = True

    def clear_active(self):
        if self.active_reference is None:
            return
        if json.loads(self.active_path.read_text()) != self.active_reference:
            raise ValueError('PostgreSQL active journal reference changed; settlement refused')
        self.active_path.unlink()
        directory = os.open(self.active_path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
        self.active_reference = None

    def resume_active(self):
        reference = json.loads(self.active_path.read_text())
        if (not isinstance(reference, dict) or reference.get('namespace') != self.args.namespace
                or reference.get('name') != 'postgres' or not isinstance(reference.get('source'), str)
                or not re.fullmatch(r'[0-9a-f]{40,64}', reference['source'])
                or 'uid' not in reference):
            raise ValueError('PostgreSQL active journal identity is unread')
        # Derive the one allowed path; a file cannot introduce another reader.
        quarantine = Path(self.args.state) / ('postgres-quarantine-' + self.args.namespace + '-' + reference['source'] + '.json')
        previous = json.loads(quarantine.read_text())
        if (not isinstance(previous, dict) or any(previous.get(key) != reference[key] for key in ('source', 'namespace', 'name'))
                or previous.get('forward_uid') != reference['uid']
                or previous.get('state') not in ('attempting', 'failed', 'ready')
                or journal_digest(previous) != reference.get('inverse_digest')):
            raise ValueError('PostgreSQL active journal digest/identity changed; recovery refused')
        self.active_reference = reference
        self.quarantine = quarantine
        self.record['reconciled_source'] = reference['source']
        live = self.live()
        if self.bootstrap_journal(previous):
            self.resume_bootstrap(previous, live)
            return
        if live is None or (reference['uid'] is not None and live['metadata']['uid'] != reference['uid']):
            raise ValueError('PostgreSQL active journal StatefulSet UID changed; recovery refused')
        pointer = Path(self.args.state) / ('postgres-last-ready-' + self.args.namespace + '.json')
        target = json.loads(pointer.read_text()) if pointer.exists() else None
        # save_ready precedes the per-head ready marker. A process can die
        # between them: do not undo a durably recorded successful forward.
        if isinstance(target, dict) and target.get('source') == reference['source']:
            good_path = Path(self.args.state) / ('postgres-ready-' + self.args.namespace + '-' + reference['source'] + '.json')
            good = json.loads(good_path.read_text())
            for row in (target, good):
                if (not isinstance(row, dict) or row.get('namespace') != self.args.namespace
                        or row.get('name') != 'postgres' or row.get('source') != reference['source']
                        or row.get('uid') != live['metadata']['uid']):
                    raise ValueError('PostgreSQL recorded forward identity changed; recovery refused')
            if not isinstance(good.get('spec'), dict) or self.merged_spec(live, good['spec']) != live['spec']:
                raise ValueError('PostgreSQL recorded forward spec changed; recovery refused')
            self.wait(live['metadata']['uid'])
            state = 'failed' if previous['state'] == 'failed' else 'ready'
            atomic(quarantine, dict(previous, state=state, uid=live['metadata']['uid'], reconciliation='forward-verified-ready'))
            self.clear_active()
            self.record['interrupted_forward'] = 'verified-ready'
            atomic(Path(self.args.receipt), self.record)
            raise ValueError('PostgreSQL quarantined prior forward verified healthy; current candidate was not attempted')
        if previous['state'] == 'ready':
            raise ValueError('PostgreSQL ready journal has no matching namespace receipt; recovery refused')
        self.reconcile(previous, live)

    def bootstrap_journal(self, previous):
        # A bootstrap forward journals no inverse: there was no StatefulSet
        # to undo, so its marker is not a target reconcile can restore
        # (backlog bd5964f2, review of eeb0fe14). Only the exact marker shape
        # written by execute_locked qualifies; anything else stays refused.
        return (previous.get('uid') is None and previous.get('forward_uid') is None
                and previous.get('rollback_target') == BOOTSTRAP_TARGET
                and previous.get('forward_inverse') is None)

    def resume_bootstrap(self, previous, live):
        """A journaled bootstrap is nothing to undo; it is settled or retried.

        Nothing rolls back or is deleted here. Created nothing: the journal
        is cleared and this tick bootstraps. Created something: the live
        StatefulSet is adopted only when it is the journaled head's own
        rendering and it becomes ready; otherwise the tick refuses and the
        journal stays for the next one.
        """
        if live is None:
            atomic(self.quarantine, dict(previous, state='failed', reconciliation='bootstrap-created-nothing'))
            self.clear_active()
            self.record['interrupted_forward'] = 'bootstrap-created-nothing'
            atomic(Path(self.args.receipt), self.record)
            return
        desired = self.rendered_postgres(previous['source'])
        if desired is None or self.merged_spec(live, desired['spec']) != live['spec']:
            raise ValueError('PostgreSQL bootstrap journal does not match the live StatefulSet; recovery refused')
        try:
            current = self.wait(live['metadata']['uid'])
        except ValueError as error:
            self.record['interrupted_forward'] = {'bootstrap-not-ready': str(error)}
            atomic(Path(self.args.receipt), self.record)
            raise
        self.save_ready({'source': previous['source'], 'namespace': self.args.namespace, 'name': 'postgres',
                         'uid': current['metadata']['uid'], 'spec': desired['spec']})
        atomic(self.quarantine, dict(previous, state='ready', uid=current['metadata']['uid'],
                                     reconciliation='bootstrap-verified-ready'))
        self.clear_active()
        self.record['interrupted_forward'] = 'bootstrap-verified-ready'
        atomic(Path(self.args.receipt), self.record)

    def reconcile(self, previous, live):
        good, inverse = previous.get('rollback_target'), previous.get('forward_inverse')
        if (live is None or not isinstance(good, dict) or not isinstance(inverse, dict)
                or previous.get('uid') != live['metadata']['uid']
                or good.get('uid') != live['metadata']['uid']
                or good.get('namespace') != self.args.namespace or good.get('name') != 'postgres'
                or not isinstance(good.get('spec'), dict)
                or not isinstance(good.get('source'), str)
                or not re.fullmatch(r'[0-9a-f]{40,64}', good['source'])
                or inverse.get('uid') != live['metadata']['uid'] or inverse.get('source') != good['source']
                or not isinstance(inverse.get('changes'), list) or not inverse['changes']):
            raise ValueError('PostgreSQL quarantined attempt has no qualified journaled inverse')
        before, after = [], []
        for change in inverse['changes']:
            if (not isinstance(change, dict) or not isinstance(change.get('path'), str)
                    or not change['path'].startswith('/spec/')
                    or not isinstance(change.get('before_present'), bool)
                    or not isinstance(change.get('after_present'), bool)):
                raise ValueError('PostgreSQL quarantined inverse is unread')
            present, value = value_at(live, change['path'])
            before.append(present == change['before_present'] and (not present or value == change.get('before')))
            after.append(present == change['after_present'] and (not present or value == change.get('after')))
        self.record.update(rollback_target=good, forward_inverse=inverse)
        try:
            if all(before):
                self.wait(live['metadata']['uid'])
            elif all(after):
                self.rollback(live)
            else:
                raise ValueError('PostgreSQL spec moved independently; interrupted rollback refused')
            self.save_ready(good)
            self.record['rollback'] = 'ready'
            self.clear_active()
        except Exception as error:
            self.record['rollback'] = {'failed': str(error)}
            raise
        finally:
            atomic(self.quarantine, dict(previous, state='failed', reconciliation=self.record.get('rollback')))
            atomic(Path(self.args.receipt), self.record)
        # Recovery is not successful delivery of the interrupted candidate.
        raise ValueError('PostgreSQL quarantined interrupted forward reconciled; candidate remains failed')

    def extract(self, directory, remove=False):
        module_spec = importlib.util.spec_from_file_location('claim_documents', Path(__file__).with_name('standalone-pvc.py'))
        module = importlib.util.module_from_spec(module_spec)
        module_spec.loader.exec_module(module)
        selected, groups = [], {}
        for row in module.documents(directory):
            is_postgres = False
            if 'kind: StatefulSet' in row['kinds']:
                result = self.kubectl(['create', '--dry-run=client', '-f', '-', '-o', 'json'], row['body'])
                obj = json.loads(result['stdout'])
                is_postgres = obj.get('kind') == 'StatefulSet' and obj.get('metadata', {}).get('name') == 'postgres'
                if is_postgres:
                    if obj['metadata'].get('namespace') != self.args.namespace:
                        raise ValueError('PostgreSQL source namespace mismatch')
                    selected.append(obj)
            if not (remove and is_postgres):
                groups.setdefault(row['path'], []).append(row)
            elif row['path'] not in groups:
                groups[row['path']] = []
        if len(selected) > 1:
            raise ValueError('ambiguous PostgreSQL source')
        if remove:
            for path, rows in groups.items():
                module.write_documents(path, rows)
        return selected[0] if selected else None

    def named_good(self, live):
        last = self.args.last
        if not re.fullmatch(r'[0-9a-f]{7,64}', last):
            raise ValueError('existing PostgreSQL has no named last-converged source')
        source = self.run(['git', '-C', self.args.repo, 'rev-parse', '--verify', last + '^{commit}'])['stdout'].strip()
        obj = self.rendered_postgres(source)
        if obj is None or self.merged_spec(live, obj['spec']) != live['spec'] or not self.ready(live):
            raise ValueError('live PostgreSQL is not the ready named last-converged spec')
        return {'source': source, 'namespace': self.args.namespace, 'name': 'postgres', 'uid': live['metadata']['uid'], 'spec': obj['spec']}

    def rendered_postgres(self, source):
        """The PostgreSQL StatefulSet one committed source renders, or None."""
        with tempfile.TemporaryDirectory(dir=self.args.state) as temporary:
            tree = Path(temporary)
            bound = self.named_source_seconds
            started = time.monotonic()
            try:
                archive = subprocess.run(['git', '-C', self.args.repo, 'archive', source, 'infra/cluster', 'examples'], capture_output=True, timeout=bound)
            except subprocess.TimeoutExpired as error:
                # In words, naming the archive and the time, never a raw
                # TimeoutExpired traceback (gate 35600cc4, backlog aa0013a5).
                elapsed = time.monotonic() - started
                self.record['named_source_export'] = {'source': source, 'exit_code': 124, 'timed_out': True,
                                                      'bound_seconds': bound, 'elapsed_seconds': round(elapsed, 1),
                                                      'stderr': (error.stderr or b'').decode(errors='replace')}
                raise ValueError('named PostgreSQL source export timed out: git archive of %s (infra/cluster, examples) '
                                 'from %s ran %.1f s against its %s s bound' % (source, self.args.repo, elapsed, bound)) from None
            self.record['named_source_export'] = {'source': source, 'exit_code': archive.returncode, 'stderr': archive.stderr.decode(errors='replace')}
            if archive.returncode:
                raise ValueError('named PostgreSQL source export failed: ' + self.record['named_source_export']['stderr'])
            with tarfile.open(fileobj=io.BytesIO(archive.stdout)) as tar:
                total = 0
                for member in tar:
                    path = Path(member.name)
                    if path.is_absolute() or '..' in path.parts or not (member.isdir() or member.isfile()):
                        raise ValueError('unsafe entry in named PostgreSQL source archive')
                    total += member.size
                    if total > 64 * 1024 * 1024:
                        raise ValueError('named PostgreSQL source export exceeds 64MiB bound')
                    destination = tree / path
                    if member.isdir():
                        destination.mkdir(parents=True, exist_ok=True)
                    else:
                        destination.parent.mkdir(parents=True, exist_ok=True)
                        with tar.extractfile(member) as contents:
                            destination.write_bytes(contents.read())
            return self.rendered_tree(tree, tree / 'rendered')

    def rendered_tree(self, tree, out):
        """The PostgreSQL StatefulSet a plain tree renders into `out`, or None.

        rendered_postgres's second half, after its git export: a tree that
        already IS its commit is rendered as it stands. The forge runs ops
        verbs from a generation of root's tree (gen/<sha>, no .git), where
        no git archive can run — read-postgres-placement answered CANNOT
        ANSWER there on its first two live runs (backlog 2cfea79b).
        """
        env = dict(os.environ, BOSS_CLUSTER_TREE=str(tree))
        renderer = str(Path(self.args.repo) / 'infra/cluster/render-instance.sh')
        self.run(['bash', renderer, '--all', str(out)], env=env, timeout=self.named_source_seconds)
        directory = Path(out) / self.args.namespace
        if not directory.is_dir():
            raise ValueError('namespace absent from last-converged source')
        return self.extract(directory)

    def ready_target(self, live):
        pointer = Path(self.args.state) / ('postgres-last-ready-' + self.args.namespace + '.json')
        if pointer.exists():
            target = json.loads(pointer.read_text())
            if not isinstance(target, dict):
                raise ValueError('unread PostgreSQL namespace ready pointer')
            source = target.get('source')
            if not isinstance(source, str) or not re.fullmatch(r'[0-9a-f]{40,64}', source):
                raise ValueError('unread PostgreSQL namespace ready pointer')
            good = json.loads((Path(self.args.state) / ('postgres-ready-' + self.args.namespace + '-' + source + '.json')).read_text())
            for row in (target, good):
                if (not isinstance(row, dict) or row.get('namespace') != self.args.namespace or row.get('name') != 'postgres'
                        or row.get('uid') != live['metadata']['uid'] or row.get('source') != source):
                    raise ValueError('PostgreSQL namespace ready receipt identity changed')
            if not isinstance(good.get('spec'), dict):
                raise ValueError('unread PostgreSQL namespace ready spec')
            return good
        # First adoption only. The source stamp cannot supersede an existing
        # namespace pointer after a skipped or failed playground rollout.
        good = self.named_good(live)
        self.save_ready(good)
        return good

    def save_ready(self, good):
        state = Path(self.args.state)
        atomic(state / ('postgres-ready-' + self.args.namespace + '-' + good['source'] + '.json'), good)
        atomic(state / ('postgres-last-ready-' + self.args.namespace + '.json'),
               {key: good[key] for key in ('source', 'namespace', 'name', 'uid')})

    def rollback(self, live):
        failed = self.fence(live)
        inverse = self.record.get('forward_inverse')
        if not isinstance(inverse, dict) or inverse.get('uid') != failed['metadata']['uid']:
            raise ValueError('PostgreSQL spec moved independently or inverse is unread; rollback refused')
        operations = [{'op': 'test', 'path': '/metadata/uid', 'value': failed['metadata']['uid']},
                      {'op': 'test', 'path': '/metadata/resourceVersion', 'value': failed['metadata']['resourceVersion']}]
        writes = []
        for change in inverse['changes']:
            present, value = value_at(failed, change['path'])
            if present != change['after_present'] or (present and value != change['after']):
                raise ValueError('PostgreSQL spec moved independently at ' + change['path'] + '; rollback refused')
            if present:
                operations.append({'op': 'test', 'path': change['path'], 'value': value})
            else:
                # JSON Patch cannot test absence. Test the observed parent;
                # UID/RV additionally fences every omitted key until this write.
                parent = change['path'].rsplit('/', 1)[0]
                operations.append({'op': 'test', 'path': parent, 'value': value_at(failed, parent)[1]})
            if change['before_present']:
                writes.append({'op': 'replace' if present else 'add', 'path': change['path'], 'value': change['before']})
            else:
                writes.append({'op': 'remove', 'path': change['path']})
        self.kubectl(['patch', 'sts/postgres', '-n', self.args.namespace, '--type=json', '-p', json.dumps(operations + writes), '--request-timeout=60s'])
        try:
            self.wait(live['metadata']['uid'])
            return
        except ValueError as first_wait:
            self.record['rollback_first_readiness_failure'] = str(first_wait)
        # StatefulSet forced rollback can retain a failed revision's Pod.
        # Only an observed restored generation can identify a stale revision.
        current = self.fence(live)
        revision = current.get('status', {}).get('updateRevision')
        failed_revision = failed.get('status', {}).get('updateRevision')
        if (not revision or current.get('status', {}).get('observedGeneration', 0)
                < current['metadata'].get('generation', 1)):
            raise ValueError('restored PostgreSQL revision is unread; Pod recycle refused')
        pods = json.loads(self.kubectl(['get', 'pods', '-n', self.args.namespace, '-o', 'json', '--request-timeout=30s'])['stdout'])
        stale = [p for p in pods['items'] if any(o.get('uid') == live['metadata']['uid'] and o.get('name') == 'postgres' and o.get('kind') == 'StatefulSet' and o.get('controller') is True for o in p['metadata'].get('ownerReferences', []))
                 and failed_revision is not None and failed_revision != revision
                 and p['metadata'].get('labels', {}).get('controller-revision-hash') == failed_revision
                 and not any(c.get('type') == 'Ready' and c.get('status') == 'True' for c in p.get('status', {}).get('conditions', []))]
        if len(stale) != 1:
            raise ValueError('rollback is not ready and no single exact failed owned stale Pod qualifies')
        pod = stale[0]
        evidence = failure_evidence(pod, datetime.now(timezone.utc))
        self.record['recycle_evidence'] = evidence
        if evidence is None:
            raise ValueError('owned stale PostgreSQL Pod has no evidence of failure (phase %s); recycle refused'
                             % pod.get('status', {}).get('phase'))
        name = pod['metadata']['name']
        if not re.fullmatch(r'[a-z0-9][a-z0-9.-]*', name):
            raise ValueError('unread PostgreSQL Pod name')
        body = {'kind': 'DeleteOptions', 'apiVersion': 'v1', 'gracePeriodSeconds': 30, 'preconditions': {'uid': pod['metadata']['uid']}}
        self.kubectl(['delete', '--raw', '/api/v1/namespaces/' + self.args.namespace + '/pods/' + name, '-f', '-', '--request-timeout=60s'], json.dumps(body))
        self.wait(live['metadata']['uid'])

    def execute(self):
        Path(self.args.state).mkdir(parents=True, exist_ok=True)
        if not re.fullmatch(r'[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?', self.args.namespace):
            raise ValueError('unread PostgreSQL namespace')
        # The lock lives only for this process, including waits and rollback.
        # Death releases it; another tick never reconciles a still-live writer.
        with (Path(self.args.state) / ('postgres-recovery-' + self.args.namespace + '.lock')).open('a') as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise ValueError('PostgreSQL namespace recovery is already active') from error
            return self.execute_locked()

    def execute_locked(self):
        Path(self.args.state).mkdir(parents=True, exist_ok=True)
        if not re.fullmatch(r'[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?', self.args.namespace):
            raise ValueError('unread PostgreSQL namespace')
        if not re.fullmatch(r'[0-9a-f]{40,64}', self.args.head):
            raise ValueError('current PostgreSQL source must be a full commit identity')
        self.active_path = Path(self.args.state) / ('postgres-active-journal-' + self.args.namespace + '.json')
        if self.active_path.exists():
            self.record['postgres_present'] = True
            self.resume_active()
        desired = self.extract(self.args.directory)
        if desired is None:
            self.record['postgres_present'] = False
            self.record['postgres_failure'] = False
            self.run(shlex.split(self.args.apply) + ['apply', '-f', self.args.mounted])
            self.record['outcome'] = 'not-applicable'
            return
        self.record['postgres_present'] = True
        live = self.live()
        quarantine = Path(self.args.state) / ('postgres-quarantine-' + self.args.namespace + '-' + self.args.head + '.json')
        self.quarantine = quarantine
        if quarantine.exists():
            previous = json.loads(quarantine.read_text())
            if (isinstance(previous, dict) and previous.get('source') == self.args.head
                    and previous.get('namespace') == self.args.namespace and previous.get('name') == 'postgres'
                    and previous.get('state') == 'attempting'):
                self.reconcile(previous, live)
            if (isinstance(previous, dict) and previous.get('source') == self.args.head
                    and previous.get('namespace') == self.args.namespace and previous.get('name') == 'postgres'
                    and previous.get('state') == 'failed' and live is None and self.bootstrap_journal(previous)):
                # A bootstrap that created nothing left nothing a failed
                # rollout could have hurt: this head may try the create
                # again (bd5964f2). Its first attempt stays in that run's receipt.
                self.record['bootstrap_retry'] = previous.get('reconciliation') or previous.get('failure')
            elif (not isinstance(previous, dict) or previous.get('source') != self.args.head or previous.get('namespace') != self.args.namespace
                    or previous.get('name') != 'postgres' or previous.get('state') != 'ready'
                    or live is None or previous.get('uid') != live['metadata']['uid']):
                raise ValueError('PostgreSQL namespace/head quarantined by an unfinished or failed attempt')
        good = None
        if live is not None:
            good = self.ready_target(live)
            self.record['rollback_target'] = good
        else:
            self.record['rollback_target'] = dict(BOOTSTRAP_TARGET)
        atomic(Path(self.args.receipt), self.record)
        # Remove only the identified PostgreSQL envelope; every other document
        # remains byte-for-byte input to the existing ordinary apply.
        self.extract(self.args.directory, remove=True)
        # Secret/config/service dependencies precede PostgreSQL bootstrap.
        # Ordinary apply never sees the isolated StatefulSet itself.
        try:
            self.run(shlex.split(self.args.apply) + ['apply', '-f', self.args.mounted])
        except ValueError:
            self.record['outcome'] = 'dependencies-failed'
            self.record['postgres_failure'] = False
            atomic(Path(self.args.receipt), self.record)
            raise
        attempt = {'source': self.args.head, 'namespace': self.args.namespace, 'name': 'postgres',
                   'uid': live['metadata']['uid'] if live is not None else None, 'state': 'attempting'}
        try:
            if live is None:
                self.journal_forward(None)
                created = json.loads(self.kubectl(['create', '-f', '-', '-o', 'json', '--request-timeout=60s'], json.dumps(desired))['stdout'])
                metadata = created.get('metadata', {})
                if (created.get('kind') != 'StatefulSet' or metadata.get('name') != 'postgres'
                        or metadata.get('namespace') != self.args.namespace or not metadata.get('uid')):
                    raise ValueError('bootstrap PostgreSQL create returned no qualified identity')
                expected_uid = metadata['uid']
            else:
                self.patch(live, desired['spec'])
                expected_uid = live['metadata']['uid']
            current = self.wait(expected_uid)
            self.save_ready({'source': self.args.head, 'namespace': self.args.namespace, 'name': 'postgres',
                             'uid': current['metadata']['uid'], 'spec': desired['spec']})
            journal = json.loads(quarantine.read_text()) if self.record.get('forward_issued') else attempt
            atomic(quarantine, dict(journal, state='ready', uid=current['metadata']['uid']))
            self.clear_active()
            self.record['outcome'] = 'ready'
            self.record['postgres_failure'] = False
        except Exception as error:
            self.record['primary_failure'] = str(error)
            self.record['outcome'] = 'failed'
            if self.record.get('forward_issued'):
                journal = json.loads(quarantine.read_text())
                atomic(quarantine, dict(journal, state='failed', failure=str(error)))
            if good is not None and self.record.get('forward_issued'):
                try:
                    self.rollback(live)
                    self.save_ready(good)
                    self.record['rollback'] = 'ready'
                    self.clear_active()
                except Exception as rollback:
                    self.record['rollback'] = {'failed': str(rollback)}
            raise
        finally:
            atomic(Path(self.args.receipt), self.record)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('directory', 'namespace', 'read', 'write', 'apply', 'mounted', 'repo', 'last', 'head', 'state', 'receipt'):
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    recovery = Recovery(args)
    try:
        recovery.execute()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        recovery.record['failure'] = str(error)
        if recovery.record['postgres_present'] is True and recovery.record['postgres_failure'] is not False:
            recovery.record['postgres_failure'] = True
        atomic(Path(args.receipt), recovery.record)
        raise SystemExit('PostgreSQL converge refused: ' + str(error))
    atomic(Path(args.receipt), recovery.record)


if __name__ == '__main__':
    main()
