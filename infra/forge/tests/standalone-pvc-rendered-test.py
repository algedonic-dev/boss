"""Permanent real-render -> namespace/stage/isolate/apply boundary pin.

No cluster, client context or credential is used. The stand-in rejects a PVC
in EVERY ordinary apply, including the real namespace-first helper, and
requires namespace creation before claim reads or creates.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT=Path(__file__).resolve().parents[3]

ADAPTER='''#!/usr/bin/env python3
import json,os,re,sys
from pathlib import Path
a=sys.argv[1:]
root=Path(os.environ['APPLY_DIR'])
state=Path(os.environ['NAMESPACE_STATE'])
created=set(json.loads(state.read_text())) if state.exists() else set()
with open(os.environ['CALLS'],'a') as f: f.write(json.dumps(a)+'\\n')
if a[0]=='create' and '--dry-run=client' in a:
    text=sys.stdin.read()
    name=re.search(r'^ +name: ([^\\n]+)$',text,re.M)[1]
    ns=re.search(r'^ +namespace: ([^\\n]+)$',text,re.M)[1]
    print(json.dumps({'kind':'StatefulSet','metadata':{'name':name,'namespace':ns},'spec':{'replicas':1}}))
    sys.exit(0)
if a[:2]==['get','sts/postgres']:
    path=root/('pg-'+a[a.index('-n')+1]+'.json')
    if path.exists(): print(path.read_text())
    sys.exit(0)
if a[0]=='rollout': sys.exit(0)
if a[0]=='create' and a[a.index('-f')+1]=='-':
    obj=json.load(sys.stdin); ns=obj['metadata']['namespace']
    assert ns in created
    obj['metadata'].update(uid='pg-'+ns,resourceVersion='1',generation=1)
    obj['status']={'observedGeneration':1,'readyReplicas':1,'updatedReplicas':1,'currentRevision':'ready','updateRevision':'ready'}
    (root/('pg-'+ns+'.json')).write_text(json.dumps(obj))
    print(json.dumps(obj))
    sys.exit(0)
if a[0] in ('apply','create'):
    supplied=Path(a[a.index('-f')+1].replace('/manifests/',str(root)+'/',1))
    files=sorted(supplied.glob('*.yaml')) if supplied.is_dir() else [supplied]
    assert files and all(p.is_file() for p in files),a
    text='\\n'.join(p.read_text() for p in files)
    if a[0]=='apply':
        assert not re.search(r'^kind: PersistentVolumeClaim\\s*$',text,re.M),'PVC reached ordinary apply: '+str(supplied)
        for doc in re.split(r'(?m)^---[ \\t]*\\r?\\n',text):
            if re.search(r'^kind: Namespace\\s*$',doc,re.M):
                name=re.search(r'^ +name: ([^\\n]+)$',doc,re.M)
                assert name,doc
                created.add(name[1])
    else:
        assert '--field-manager=kubectl-client-side-apply' in a,a
        assert re.search(r'^kind: PersistentVolumeClaim\\s*$',text,re.M),text
        ns=re.search(r'^ +namespace: ([^\\n]+)$',text,re.M)
        assert ns and ns[1] in created,('create before namespace',a,created)
    state.write_text(json.dumps(sorted(created)))
    sys.exit(0)
assert a[0]=='get' and a[1]=='pvc',a
ns=a[a.index('-n')+1]; name=a[2]
assert ns in created,('read before namespace creation',a,created)
if os.environ['CLAIM_STATE']=='absent': sys.exit(0)
print(json.dumps({'kind':'PersistentVolumeClaim','metadata':{'namespace':ns,'name':name,'uid':'fixture-claim','resourceVersion':'7'},
 'spec':{'resources':{'requests':{'storage':'40Gi'}}},'status':{'capacity':{'storage':'40Gi'}}}))
'''


class RenderedBoundary(unittest.TestCase):
    def exercise(self, state, empty_instance=False, bad_instance=False):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d); rendered=root/'rendered'; apply=root/'apply'; apply.mkdir()
            render=subprocess.run(['bash',str(ROOT/'infra/cluster/render-instance.sh'),'--all',str(rendered)],capture_output=True,text=True,timeout=30)
            self.assertEqual(render.returncode,0,render.stderr)
            roster=subprocess.run(['bash',str(ROOT/'infra/cluster/render-instance.sh'),'--instances'],capture_output=True,text=True,timeout=30)
            self.assertEqual(roster.returncode,0,roster.stderr)
            namespaces=[line.split('\t')[1] for line in roster.stdout.splitlines()]
            self.assertTrue(namespaces,'an empty render is not a passing pin')
            self.assertEqual(set(namespaces),{p.name for p in rendered.iterdir() if p.is_dir()})
            if bad_instance:
                bad=rendered/'boss-playground'/'boss.yaml'
                self.assertTrue(bad.is_file())
                text=bad.read_text()
                changed,n=re.subn(r'(?m)^(      storage: )[^\n]+',r'\1unread',text,count=1)
                self.assertEqual(n,1)
                bad.write_text(changed)
            if empty_instance:
                empty=rendered/'boss-empty'; empty.mkdir()
                (empty/'namespace.yaml').write_text('kind: Namespace\nmetadata:\n  name: boss-empty\n')
                namespaces.append('boss-empty')
            adapter=root/'kubectl'; adapter.write_text(ADAPTER); adapter.chmod(0o700)
            source=(ROOT/'infra/forge/cluster-deploy-runner.sh').read_text()
            functions='\n'.join(re.search(r'^'+name+r'\(\) \{\n.*?^\}',source,re.M|re.S).group() for name in ('apply_namespaces','apply_instance'))
            # Execute the real runner's stage/cleanup/apply ordering, rather
            # than keeping the rendered roster alive by construction.
            actions=[(source.index('manifests_with_image "$RENDER_DIR/'),
                      'for source in "$RENDER_DIR"/*; do ns=${source##*/}; manifests_with_image "$source" "$APPLY_DIR/$ns" fixture.invalid/boss none; done\n'),
                     (source.index('rm -rf "$RENDER_DIR"'), 'rm -rf "$RENDER_DIR"\n'),
                     (source.index('apply_instance "$SOURCE_NS"'),
                      'apply_instance "$SOURCE_NS"; echo "namespace-complete:$SOURCE_NS"\n'),
                     (source.index('apply_instance "$ins_ns"'),
                      'for ns in '+ ' '.join(namespaces) + '; do [ "$ns" = "$SOURCE_NS" ] && continue; apply_instance "$ns"; echo "namespace-complete:$ns"; done\n')]
            script=root/'run.sh'
            script.write_text('set -eu\n. "$REPO/infra/run-summary.sh"\n. "$REPO/infra/forge/cluster-deploy-lib.sh"\n'+functions+'\n'
                              + ''.join(command for _,command in sorted(actions)))
            env=dict(os.environ,REPO=str(ROOT),APPLY_DIR=str(apply),RENDERED=str(rendered),RENDER_DIR=str(rendered),K=str(adapter),KM=str(adapter),
                     KAPPLY=str(adapter),CONVERGE_STATE=str(root),HEAD_FULL='1'*40,LAST='none',
                     CALLS=str(root/'calls'),NAMESPACE_STATE=str(root/'namespaces.json'),CLAIM_STATE=state,SOURCE_NS=namespaces[0],
                     JOBS_API='',BOSS_JOBS_URL='',BOSS_SOR_ENV=str(root/'absent-sor.env'),BOSS_RUN_SUMMARY_FILE=str(root/'summary.json'))
            result=subprocess.run(['bash',str(script)],env=env,capture_output=True,text=True,timeout=60)
            self.assertEqual(result.returncode,0,result.stdout+'\n'+result.stderr)
            self.assertEqual(sorted(line.split(':',1)[1] for line in result.stdout.splitlines() if line.startswith('namespace-complete:')),sorted(namespaces))
            calls=[json.loads(line) for line in (root/'calls').read_text().splitlines()]
            self.assertTrue(any(a[:2]==['get','pvc'] for a in calls),'no claim read is not a passing pin')
            if bad_instance:
                self.assertFalse(any(a[:2]==['get','pvc'] and a[a.index('-n')+1]=='boss-playground' for a in calls))
                summary=json.loads((root/'summary.json').read_text())
                self.assertIn('instance boss-playground',summary['anomalies'])
            self.assertEqual(any(a[0]=='create' and '--field-manager=kubectl-client-side-apply' in a for a in calls),state=='absent')
            if state=='existing':
                known=0
                for ns in namespaces:
                    rows=json.loads((apply/f'standalone-pvc-{ns}.receipt.json').read_text())
                    known+=len(rows)
                    if bad_instance and ns=='boss-playground':
                        self.assertTrue(all(r['apply_state']=='held_back_stage_refusal' for r in rows))
                        continue
                    self.assertTrue(all(r.get('apply_state')=='held_back_existing' and r.get('held_back_document') for r in rows),rows)
                    for row in rows:
                        if row['claim']=='boss-files':
                            if bad_instance:
                                self.assertFalse(row['affected_claims_complete'])
                                self.assertEqual(row['rendered_roster_refusals'][0]['instance'],'boss-playground')
                                continue
                            self.assertEqual({v['namespace'] for v in row['affected_claims']},
                                             {'boss','boss-playground'})
                self.assertGreater(known,0,'no existing claims were covered')

    def test_instance_without_claims_does_not_weaken_nonempty_global_coverage(self):
        self.exercise('existing',empty_instance=True)

    def test_bad_instance_does_not_hold_healthy_instances_or_hide_partial_attribution(self):
        self.exercise('existing',bad_instance=True)

    def test_real_rendered_existing_claims_never_reach_ordinary_apply(self):
        self.exercise('existing')

    def test_first_deploy_creates_namespaces_before_missing_claims(self):
        self.exercise('absent')


if __name__=='__main__': unittest.main()
