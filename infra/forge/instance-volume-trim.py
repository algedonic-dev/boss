#!/usr/bin/env python3
"""Trim ONE instance volume's filesystem, plan first: backlog b1520f80 part (b).

WHY. On 2026-10-29 the system of record's PostgreSQL volume held 99.6 GiB
actual for 36.7 GB of data, and on 2026-11-15 boss-playground's
pgdata-postgres-0 held 31.2 GiB actual on a 20 GiB claim, driving
disk_tight:cp-2. A filesystem never hands freed blocks back to Longhorn
without a trim, and check-node-trim covers only the build node's own
filesystems. Part (a), the replica history reader, landed in #1486; the
snapshot purge already has its passkey pair (#1389). This is the rest:
Longhorn's own trimFilesystem action on one named claim, under David's
per-run passkey on a rendered plan, with status.actualSize read back
before and after.

WHAT LONGHORN DOES (longhorn-manager v1.11.3 manager/volume.go
TrimFilesystem): refuses a Block-mode claim, a volume not attached, a
disabled frontend; for a non-RWX volume runs fstrim on the node's mount
of /dev/longhorn/<volume>. The recurring filesystem-trim job then purges
snapshots (app/recurringjob/volume.go doRecurringFilesystemTrim); this
verb does NOT — the purge is its own approved pair.

THE ADMISSION is the purge's, whole (instance-snapshot-purge.py qualify:
exact PVC UID -> PV CSI handle -> Longhorn UID, engine v1.11.3, the
complete healthy RW replica set, no active or unknown backup, rebuild,
restore, clone or purge, no unfinished claim Job), plus what only a trim
reads: a Filesystem-mode claim, a rwo blockdev volume with its frontend
enabled and no encryption (the only shape these controls exercise), a
Running pod holding the claim on the node the volume is attached to (the
mounted filesystem fstrim needs), and Longhorn's
unmapMarkSnapChainRemoved resolving to disabled — enabled, a trim marks
the snapshot chain removed, which is a snapshot decision this verb
refuses to make.
"""
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import sys
import tempfile
import time
from urllib.parse import urlencode

_spec = importlib.util.spec_from_file_location('instance_snapshot_purge', Path(__file__).with_name('instance-snapshot-purge.py'))
base = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(base)
require, identity, digest, LH = base.require, base.identity, base.digest, base.LH

SETTING = 'remove-snapshots-during-filesystem-trim'
# fstrim answers when it is done; a 100 GiB volume is minutes, not seconds.
TRIM_SECONDS = 900
# Longhorn refreshes status.actualSize from the engine on its own cadence;
# the read-back takes a fixed series rather than guessing when it settled.
SETTLE_SECONDS = 60
INTERVAL_SECONDS = 10


def resolve(volume, setting):
    one_setting = base.one(setting, 'Setting', SETTING, LH)
    value = one_setting.get('value')
    require(value in ('true', 'false'), 'unread ' + SETTING + ' value')
    field = volume['spec'].get('unmapMarkSnapChainRemoved')
    require(field in ('ignored', 'enabled', 'disabled'), 'unread unmapMarkSnapChainRemoved')
    resolved = field if field != 'ignored' else ('enabled' if value == 'true' else 'disabled')
    require(resolved == 'disabled', 'a trim here would mark the snapshot chain removed (unmapMarkSnapChainRemoved '
            + field + ', ' + SETTING + ' ' + value + '); that is a snapshot decision, not this verb')
    return {'volume': field, 'setting': value, 'resolved': resolved}


def mounted_by(b, pvc, node):
    rows = []
    for pod in b['pods']:
        if base.holds(pod['spec'], pvc) and pod.get('status', {}).get('phase') == 'Running' and pod['spec'].get('nodeName') == node:
            rows.append({**identity(pod), 'node': node})
    require(rows, 'no Running pod holds the claim on attached node ' + str(node) + '; no mounted filesystem to trim')
    return sorted(rows, key=lambda x: x['name'])


def replica_rows(replicas, engine):
    return sorted([{**identity(r), 'node': r['spec']['nodeID'], 'disk': r['spec']['diskID'],
                    'address': engine['spec']['replicaAddressMap'][r['metadata']['name']]} for r in replicas],
                  key=lambda x: x['name'])


def render(b, namespace, pvc):
    p, pv, v, replicas, e, _, _ = base.qualify(b, namespace, pvc)
    s, st = v['spec'], v['status']
    require(p['spec'].get('volumeMode') == 'Filesystem', 'claim is not Filesystem mode; Longhorn trims only a mounted filesystem')
    require(s.get('accessMode') == 'rwo' and s.get('frontend') == 'blockdev' and s.get('disableFrontend') is False
            and st.get('frontendDisabled') is False and s.get('encrypted') is False,
            'volume is not a rwo blockdev with its frontend enabled and no encryption')
    require(isinstance(s.get('size'), str) and re.fullmatch(r'[1-9][0-9]*', s['size']), 'unread volume size')
    return {'schema': 1, 'operation': 'trimFilesystem', 'claim': {**identity(p), 'namespace': namespace},
            'pv': identity(pv), 'volume': {**identity(v), 'size': s['size'], 'access_mode': s['accessMode'], 'node': s['nodeID']},
            'engine': {**identity(e), 'image': e['status']['currentImage']}, 'replicas': replica_rows(replicas, e),
            'mounted_by': mounted_by(b, pvc, s['nodeID']),
            'unmap_mark_snap_chain_removed': resolve(v, b['setting']),
            'scope': 'fstrim of the mounted filesystem through Longhorn trimFilesystem; no snapshot purge, deletion or mark, no resize, attach or setting change',
            'limits': 'fresh reads are not a cross-resource lock; blocks a snapshot still holds are not released; bytes reclaimed are read back, never promised'}


def actual(volume):
    size = volume.get('status', {}).get('actualSize')
    require(type(size) is int and size >= 0, 'unread status.actualSize')
    return size


def execute(read, action, namespace, pvc, approved, **options):
    attempted = False
    def fixed_action(volume):
        nonlocal attempted
        attempted = True
        return action(volume)
    try:
        return run_trim(read, fixed_action, namespace, pvc, approved, **options)
    except (ValueError, KeyError, TypeError, AttributeError, OSError, StopIteration) as error:
        if attempted:
            raise base.Unproven('trim requested; effect NOT proven: ' + str(error)) from error
        raise


def run_trim(read, action, namespace, pvc, approved, poll=None, sleep=time.sleep):
    plan = render(read(), namespace, pvc)
    require(digest(plan) == approved, 'approved plan differs; nothing was trimmed')
    # A second independent inventory immediately precedes the one action,
    # and its actualSize is the "before".
    fresh = read()
    require(render(fresh, namespace, pvc) == plan, 'plan moved before trim; nothing was trimmed')
    before = actual(fresh['volume'])
    action(plan['volume']['name'])
    readings = []
    for _ in range(SETTLE_SECONDS // INTERVAL_SECONDS):
        sleep(INTERVAL_SECONDS)
        after = poll(plan) if poll is not None else read()
        p, pv, v, replicas, engine, _, _ = base.qualify(after, namespace, pvc, during=True)
        require(identity(p) == {k: plan['claim'][k] for k in ('name', 'uid')} and identity(pv) == plan['pv']
                and identity(v) == {k: plan['volume'][k] for k in ('name', 'uid')}
                and identity(engine) == {k: plan['engine'][k] for k in ('name', 'uid')}
                and replica_rows(replicas, engine) == plan['replicas'],
                'binding/engine/replica identity moved after trim; effect unknown')
        readings.append(actual(v))
    return {'state': 'trim-acknowledged', 'plan_sha256': approved, 'volume': plan['volume'],
            'actual_size_before': before, 'actual_size_after': readings[-1], 'readings_after': readings,
            'settle_seconds': SETTLE_SECONDS, 'capacity_recovery_proven': False,
            'attribution': 'Longhorn status.actualSize, one replica of data; another writer or a concurrent trim cannot be told apart',
            'next': 'read fresh per-disk available bytes (node-disk-usage) and the estate comparison separately'}


class Native(base.Native):
    def trim(self, volume):
        require(isinstance(volume, str) and re.fullmatch(base.VOLUME, volume), 'outside fixed action roster')
        self.mutation_attempted = True
        answer = self.call(['create', '--raw', base.PROXY + volume + '?' + urlencode({'action': 'trimFilesystem'}),
                            '-f', '/dev/null', '--request-timeout=' + str(TRIM_SECONDS) + 's'], seconds=TRIM_SECONDS)
        require(answer.get('type') == 'volume' and answer.get('name') == volume, 'trim acknowledgement is not the named volume')
        return answer

    def read(self, namespace, pvc):
        p = self.get('pvc', pvc, namespace)
        pv_name = p.get('spec', {}).get('volumeName', '')
        require(re.fullmatch(base.bounded.DNS, pv_name), 'unread PV name')
        pv = self.get('pv', pv_name, '')
        volume = pv.get('spec', {}).get('csi', {}).get('volumeHandle', '')
        require(re.fullmatch(base.VOLUME, volume), 'unread Longhorn volume handle')
        result = {'pvc': p, 'pv': pv, 'volume': self.get('volumes.longhorn.io', volume, LH)}
        for key, resource, kind, ns in [('replicas', 'replicas.longhorn.io', 'Replica', LH), ('engines', 'engines.longhorn.io', 'Engine', LH),
                                       ('backups', 'backups.longhorn.io', 'Backup', LH),
                                       ('jobs', 'jobs', 'Job', namespace), ('pods', 'pods', 'Pod', namespace)]:
            result[key] = self.listing(resource, kind, ns)
        result['setting'] = self.get('settings.longhorn.io', SETTING, LH)
        return result


def main():
    args = sys.argv[1:]; planning = bool(args and args[0] == '--plan')
    if planning:
        args = args[1:]
    require(len(args) == (2 if planning else 3), 'usage: --plan <instance> <claim> OR <instance> <claim> <approved-sha256>')
    namespace, pvc = args[:2]
    require(all(re.fullmatch(base.bounded.DNS, x) for x in (namespace, pvc)), 'invalid namespace or claim')
    require(namespace in os.environ.get('INSTANCE_TRIM_NAMESPACES', '').split(), 'namespace is not a declared instance')
    if not planning:
        require(re.fullmatch(r'[a-f0-9]{64}', args[2]), 'approved hash is not sha256')
    kubectl = shlex.split(os.environ.get('INSTANCE_TRIM_KUBECTL', ''))
    require(kubectl, 'trusted kubectl door unavailable')
    request = os.environ.get('OPS_REQUEST_ID', '')
    require(re.fullmatch(base.bounded.UUID, request), 'ops request identity is required before any native read')
    # Failed publication leaves these originals for rescue; success removes
    # them only after every part and the manifest have verified native ACKs.
    work = Path(tempfile.mkdtemp(prefix='instance-trim-'))
    print('native-originals: ' + str(work), file=sys.stderr)
    native = Native(kubectl, work, seconds=180 if planning else TRIM_SECONDS + 300)
    outcome = {'state': 'unfinished'}
    try:
        if planning:
            inventory = native.read(namespace, pvc)
            plan = render(inventory, namespace, pvc)
            outcome = {'state': 'planned', 'plan_sha256': digest(plan), 'plan': plan, 'actual_size': actual(inventory['volume'])}
        else:
            outcome = execute(lambda: native.read(namespace, pvc), native.trim, namespace, pvc, args[2], poll=native.poll)
    except (ValueError, KeyError, TypeError, AttributeError, OSError) as error:
        outcome = {'state': 'refused-or-unproven', 'reason': str(error)}
        raise
    finally:
        try:
            base.publish(work, request, outcome, label='trim')
        except (ValueError, OSError, KeyError, TypeError, AttributeError) as error:
            if native.mutation_attempted:
                raise base.Unproven('trim requested; evidence publication NOT proven: ' + str(error)) from error
            raise
        shutil.rmtree(work)
    if planning:
        print(json.dumps(plan, sort_keys=True, indent=2))
        print('plan-sha256: ' + digest(plan), file=sys.stderr)
        # A live figure, so never in the signed bytes.
        print('actual-size: ' + str(outcome['actual_size']) + ' bytes (status.actualSize at this render, not signed)', file=sys.stderr)
    else:
        # One line: the wrapper prints the effect from exactly this record.
        print(json.dumps(outcome, sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except base.Unproven as error:
        print('instance-volume-trim: FAILED — ' + str(error), file=sys.stderr)
        sys.exit(1)
    except (ValueError, KeyError, TypeError, AttributeError, OSError, StopIteration) as error:
        print('instance-volume-trim: REFUSED or CANNOT ANSWER — ' + str(error), file=sys.stderr)
        sys.exit(78)
