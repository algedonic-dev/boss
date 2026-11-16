"""READ the live PostgreSQL StatefulSet's placement beside the converge's rendering of it.

The python half of the ops verb `read-postgres-placement`
(infra/forge/postgres-placement.sh, which bounds the namespace and opens the
door; backlog eb7c4e0b). It never writes.

WHY (backlog eb7c4e0b, 2026-11-16). The review of the postgres-rollout
forward fix (car 973c1488, backlog 9123d811) needed the live placement of
BOTH postgres StatefulSets: whether either carries an affinity, nodeSelector
or tolerations key the rendering lacks, because the first converge after
that fix lands removes it and rolls the pod. dev-session may not get a
StatefulSet in boss-playground, so the playground stayed NOT OBSERVED, and
no actor below David could read it. This reads it through the forge's
ops_kubectl door, the one the other read-* verbs use.

THE RENDERING IS THE CONVERGE'S OWN, not a copy of it: postgres-rollout.py
is imported and its Recovery reads the live object (`live`) and renders the
tree this run started from (`rendered_tree`, the half of `rendered_postgres`
after its git export, which named_good uses to judge the last-converged
spec) — render-instance.sh --all and `extract`'s client-side
`create --dry-run=client` serialization. The comparison is that file's
`changes`. So when the converge's rendering moves, this reading moves with
it. The tree is rendered as it stands, never exported through git: on the
forge it is a generation of root's tree with no .git (backlog 2cfea79b),
and the shell names the commit it is.

PRUNE names each placement key the live pod template holds and the rendering
lacks, at every dict depth, as a JSON pointer under spec.template.spec. On
origin/main today the forward is a strategic merge and KEEPS such a key, so
the merged template carries both pins (converge stop c701480c); the fix car
removes exactly these on its first converge. Lists are atomic in both (a
toleration list is replaced whole), so a differing list is a change, not a
prune, and is printed as one.

NO EVIDENCE IS NOT A PASS: a read that could not look, an empty answer, an
object that is not the namespace's postgres StatefulSet, or a rendering that
carries no postgres is CANNOT ANSWER and exit 1.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import sys

ME = 'read-postgres-placement'

# The pod-template fields the rollout's placement check reads (`unfit` in
# postgres-rollout.py). Car 973c1488 names the same three PLACEMENT_FIELDS in
# that file; it is not on origin/main as this lands, so the tuple is here.
FIELDS = ('affinity', 'nodeSelector', 'tolerations')


def rollout_module():
    path = Path(__file__).with_name('postgres-rollout.py')
    spec = importlib.util.spec_from_file_location('postgres_rollout', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def placement(obj, what):
    template = (obj.get('spec') or {}).get('template')
    pod = template.get('spec') if isinstance(template, dict) else None
    if not isinstance(pod, dict):
        raise ValueError(what + ' carries no readable spec.template.spec')
    return {field: pod[field] for field in FIELDS if field in pod}


def keys(fields):
    return ','.join(field for field in FIELDS if field in fields) or 'none'


def text(value):
    """One value as one line of JSON, in the order it was read."""
    return json.dumps(value, separators=(',', ':'))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('namespace', 'read', 'write', 'repo', 'source', 'tree', 'work'):
        parser.add_argument('--' + name, required=True)
    given = parser.parse_args()
    module = rollout_module()
    work = Path(given.work)
    # Recovery's own arguments: the read door for `get`, the stdin door for
    # extract's client-side serialization, and a scratch state and receipt.
    # No apply, mount or head-to-converge: nothing here reaches a write.
    args = argparse.Namespace(namespace=given.namespace, read=given.read, write=given.write,
                              repo=given.repo, state=str(work), receipt=str(work / 'receipt.json'),
                              head=given.source, last=given.source,
                              directory=None, apply=None, mounted=None)
    recovery = module.Recovery(args)
    try:
        live = recovery.live()
        if live is None:
            raise ValueError('the read of sts/postgres in %s answered no object (NotFound, or an empty answer '
                             'this read cannot tell from it); an absence here is not a placement' % given.namespace)
        metadata, status = live['metadata'], live.get('status') or {}
        print('%s: live sts/postgres in %s: uid %s, resourceVersion %s, generation %s, observedGeneration %s, '
              'currentRevision %s, updateRevision %s' % (
                  ME, given.namespace, metadata['uid'], metadata['resourceVersion'], metadata.get('generation'),
                  status.get('observedGeneration'), status.get('currentRevision'), status.get('updateRevision')))
        have = placement(live, 'the live StatefulSet')
        for field in FIELDS:
            print('%s: live %s: %s' % (ME, field, text(have[field]) if field in have else 'absent'))
        rendered = recovery.rendered_tree(Path(given.repo), work / 'rendered')
        if rendered is None:
            raise ValueError('the rendering of %s carries no postgres StatefulSet for %s' % (given.source, given.namespace))
        want = placement(rendered, 'the rendered StatefulSet')
        for field in FIELDS:
            print('%s: rendered %s: %s' % (ME, field, text(want[field]) if field in want else 'absent'))
    except (ValueError, KeyError, OSError, subprocess.SubprocessError) as error:
        print('%s: FAILED — CANNOT ANSWER — %s — an unread placement has no answer' % (ME, error), file=sys.stderr)
        return 1
    prune = []
    for row in module.changes(have, want, path=''):
        where = row['path'].lstrip('/')
        if row['before_present'] and not row['after_present']:
            prune.append(where)
            print('%s: prune %s: live %s, absent from the rendering' % (ME, where, text(row['before'])))
        elif row['after_present'] and not row['before_present']:
            print('%s: add %s: rendered %s, absent live' % (ME, where, text(row['after'])))
        else:
            print('%s: change %s: live %s, rendered %s' % (ME, where, text(row['before']), text(row['after'])))
    print('%s: rendered from %s (%s, through postgres-rollout.py rendered_tree)' % (ME, given.source, given.tree))
    print('%s: READ — %s live=%s rendered=%s prune=%s' % (
        ME, given.namespace, keys(have), keys(want), ','.join(prune) or 'none'))
    return 0


if __name__ == '__main__':
    sys.exit(main())
