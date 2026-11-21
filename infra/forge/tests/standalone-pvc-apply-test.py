"""Exercise the real instance apply boundary without a cluster or credentials."""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
CLAIM = '''apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: boss-files
  namespace: boss
spec:
  resources:
    requests:
      storage: 20Gi
'''
OTHER = '# preserve these bytes\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: unrelated\ndata:\n  untouched: yes\n'


class ApplyBoundary(unittest.TestCase):
    def exercise(self, root, absent=False, create_refusal=False, pending=False, read_refusal=False, second=False, claim=CLAIM):
        stage = root / 'boss'; stage.mkdir()
        (stage / 'boss.yaml').write_text(OTHER + '---\n' + claim)
        if second:
            (stage/'boss-backup.yaml').write_text(CLAIM.replace('boss-files','boss-backups').replace('20Gi','40Gi'))
        live = {'apiVersion':'v1','kind':'PersistentVolumeClaim',
                'metadata':{'name':'boss-files','namespace':'boss','uid':'claim-one','resourceVersion':'7'},
                'spec':{'resources':{'requests':{'storage':'60Gi'}}},
                'status':{'capacity':{'storage':'40Gi'}}}
        if pending:
            live['status'] = {'phase':'Pending'}
        (root / 'live.json').write_text('' if absent else json.dumps(live))
        adapter = root / 'kubectl'
        adapter.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
a=sys.argv[1:]
with open(os.environ['CALLS'], 'a') as log: log.write(json.dumps(a)+'\\n')
if a[0] in ('apply','create'):
    supplied=Path(a[a.index('-f')+1].replace('/manifests/boss',os.environ['STAGE'],1))
    documents=list(supplied.glob('*.yaml')) if supplied.is_dir() else [supplied]
    assert documents and all(p.is_file() for p in documents)
    actual='\\n'.join(p.read_text() for p in documents)
    if a[0]=='apply': assert 'kind: PersistentVolumeClaim' not in actual
    else: assert 'kind: PersistentVolumeClaim' in actual
if a[0]=='get':
    if os.environ['READ_REFUSE']=='yes': sys.exit(23)
    print(Path(os.environ['LIVE']).read_text(),end=''); sys.exit(0)
if a[0]=='create':
    assert '--field-manager=kubectl-client-side-apply' in a
    sys.exit(19 if os.environ['CREATE_REFUSE']=='yes' and 'boss--boss-files.yaml' in str(supplied) else 0)
if a[0]=='apply':
    # Ordinary apply must never see an existing or new claim.
    assert 'kind: PersistentVolumeClaim' not in (Path(os.environ['STAGE'])/'boss.yaml').read_text()
    sys.exit(0)
sys.exit(2)
''')
        adapter.chmod(0o700)
        source = (ROOT / 'infra/forge/cluster-deploy-runner.sh').read_text()
        function = re.search(r'^apply_instance\(\) \{\n.*?^\}', source, re.M | re.S).group()
        script = root / 'run.sh'
        script.write_text('set -eu\n. "$REPO/infra/run-summary.sh"\n. "$REPO/infra/forge/cluster-deploy-lib.sh"\napply_namespaces() { echo namespaces >> "$ORDER"; }\n' + function + '\napply_instance boss\n')
        env = dict(os.environ, REPO=str(ROOT), APPLY_DIR=str(root), K=str(adapter), KM=str(adapter),
                   KAPPLY=str(adapter), CONVERGE_STATE=str(root), HEAD_FULL='1'*40, LAST='none',
                   CALLS=str(root/'calls'), LIVE=str(root/'live.json'), STAGE=str(stage),
                   CREATE_REFUSE='yes' if create_refusal else 'no', ORDER=str(root/'order'),
                   READ_REFUSE='yes' if read_refusal else 'no',
                   BOSS_RUN_SUMMARY_FILE=str(root/'summary.json'), JOBS_API='', BOSS_JOBS_URL='',
                   BOSS_SOR_ENV=str(root/'absent-sor.env'))
        result = subprocess.run(['bash',str(script)],env=env,capture_output=True,text=True,timeout=30)
        calls = [json.loads(x) for x in (root/'calls').read_text().splitlines()] if (root/'calls').exists() else []
        return result, calls, stage

    def test_existing_storage_never_reaches_any_apply_document(self):
        with tempfile.TemporaryDirectory() as d:
            result,calls,stage=self.exercise(Path(d))
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual([a[0] for a in calls],['get','apply'])
            self.assertEqual((stage/'boss.yaml').read_text(),OTHER)

    def test_four_space_metadata_uses_the_inventory_document_boundary(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)
            result,calls,stage=self.exercise(root,claim=CLAIM.replace('\n  ','\n    '))
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual([a[0] for a in calls],['get','apply'])
            self.assertEqual((stage/'boss.yaml').read_text(),OTHER)
            rows=json.loads((root/'standalone-pvc-boss.receipt.json').read_text())
            self.assertEqual([r['apply_state'] for r in rows],['held_back_existing'])
            self.assertEqual((stage/'standalone-existing/boss--boss-files.yaml').read_text(),CLAIM.replace('\n  ','\n    '))

    def test_claim_kind_scalar_cannot_fall_through_to_ordinary_apply(self):
        for kind in ('PersistentVolumeClaim # claim', '"PersistentVolumeClaim"'):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as d:
                root=Path(d)
                result,calls,stage=self.exercise(root,claim=CLAIM.replace('kind: PersistentVolumeClaim','kind: '+kind))
                self.assertEqual(result.returncode,0,result.stderr)
                self.assertEqual([a[0] for a in calls],['get','apply'])
                self.assertEqual((stage/'boss.yaml').read_text(),OTHER)
                rows=json.loads((root/'standalone-pvc-boss.receipt.json').read_text())
                self.assertEqual([r['apply_state'] for r in rows],['held_back_existing'])

    def test_whole_stage_refusal_withholds_claims_and_applies_unrelated_bytes(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)
            result,calls,stage=self.exercise(root,claim=CLAIM.replace('storage: 20Gi','storage: unread'))
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual([a[0] for a in calls],['apply'])
            self.assertEqual((stage/'boss.yaml').read_text(),OTHER)
            rows=json.loads((root/'standalone-pvc-boss.receipt.json').read_text())
            self.assertTrue(any(r.get('apply_state')=='held_back_stage_refusal' for r in rows),rows)
            summary=json.loads((root/'summary.json').read_text())
            self.assertIn('standalone PVC stage refused',summary['anomalies'])
            self.assertIn('boss',summary['anomalies'])

    def test_duplicate_identity_stage_refusal_never_creates_or_applies_claims(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)
            result,calls,stage=self.exercise(root,claim=CLAIM+'---\n'+CLAIM)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual([a[0] for a in calls],['apply'])
            self.assertEqual((stage/'boss.yaml').read_text(),OTHER)
            rows=json.loads((root/'standalone-pvc-boss.receipt.json').read_text())
            self.assertEqual(len(rows),2)
            self.assertTrue(all(r['apply_state']=='held_back_stage_refusal' for r in rows))
            self.assertEqual(len(list((stage/'standalone-refused').glob('*.yaml'))),2)

    def test_pending_capacity_is_unread_without_drift_or_blocking_namespace(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d); result,calls,_=self.exercise(root,pending=True)
            self.assertEqual(result.returncode,0,result.stderr)
            row=json.loads((root/'standalone-pvc-boss.receipt.json').read_text())[0]
            self.assertFalse(row['drift'])
            self.assertEqual(row['capacity_state'],'unread')
            self.assertEqual([a[0] for a in calls],['get','apply'])

    def test_per_claim_read_failure_is_recorded_without_blocking_namespace(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d); result,calls,_=self.exercise(root,read_refusal=True)
            self.assertEqual(result.returncode,0,result.stderr)
            row=json.loads((root/'standalone-pvc-boss.receipt.json').read_text())[0]
            self.assertEqual(row['read_exit_code'],23)
            self.assertFalse(row['drift'])
            self.assertEqual([a[0] for a in calls],['get','apply'])

    def test_existing_claim_is_audit_only_and_preserves_mixed_bytes(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d); result,calls,stage=self.exercise(root)
            self.assertEqual(result.returncode,0,result.stderr)
            apply=[a for a in calls if a[0]=='apply']
            self.assertEqual(len(apply),1)
            self.assertNotIn('--server-side',apply[0])
            self.assertEqual((stage/'boss.yaml').read_text(),OTHER)
            claims=list((stage/'standalone-existing').glob('*.yaml'))
            self.assertEqual(len(claims),1)
            self.assertEqual(claims[0].read_text(),CLAIM)
            row=json.loads((root/'standalone-pvc-boss.receipt.json').read_text())[0]
            self.assertEqual(row['apply_state'],'held_back_existing')
            self.assertEqual(row['requested'],'60Gi')
            self.assertTrue(row['held_back_document'])
            self.assertEqual((root/'order').read_text(),'namespaces\n')

    def test_absent_claim_is_create_only_so_a_concurrent_creator_cannot_be_overwritten(self):
        with tempfile.TemporaryDirectory() as d:
            result,calls,stage=self.exercise(Path(d),absent=True)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual([a[0] for a in calls],['get','create','apply'])
            self.assertNotIn('PersistentVolumeClaim',(stage/'boss.yaml').read_text())
            self.assertEqual(len(list((stage/'standalone-new').glob('*.yaml'))),1)
            self.assertEqual((stage/'standalone-new/boss--boss-files.yaml').read_text(),CLAIM)

    def test_concurrent_creation_refusal_is_recorded_and_namespace_continues(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d); result,calls,_=self.exercise(root,absent=True,create_refusal=True)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual([a[0] for a in calls],['get','create','apply'])
            row=json.loads((root/'standalone-pvc-boss.receipt.json').read_text())[0]
            self.assertEqual(row['create_outcome']['exit_code'],19)
            self.assertEqual(row['create_outcome']['state'],'refused')

    def test_one_claim_refusal_does_not_skip_another_missing_claim(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d); result,calls,_=self.exercise(root,absent=True,create_refusal=True,second=True)
            self.assertEqual(result.returncode,0,result.stderr)
            rows=json.loads((root/'standalone-pvc-boss.receipt.json').read_text())
            outcomes={r['claim']:r['create_outcome']['exit_code'] for r in rows}
            self.assertEqual((root/'boss/standalone-new/boss--boss-backups.yaml').read_text(),
                             CLAIM.replace('boss-files','boss-backups').replace('20Gi','40Gi'))
            self.assertEqual(outcomes,{'boss-files':19,'boss-backups':0})
            self.assertEqual(len([a for a in calls if a[0]=='create']),2)
            self.assertEqual(calls[-1][0],'apply')


if __name__=='__main__': unittest.main()
