"""Standalone claim declarations and live preservation, without a cluster."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
import subprocess
import re
import copy
import json
from unittest import mock

ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location('standalone_pvc', ROOT / 'infra/forge/standalone-pvc.py')
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

DOCUMENT = '''apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: files
  namespace: boss
spec:
  resources:
    requests:
      storage: 20Gi
'''


def live(request='40Gi', capacity='40Gi', name='files'):
    return {'apiVersion': 'v1', 'kind': 'PersistentVolumeClaim',
            'metadata': {'name': name, 'namespace': 'boss', 'uid': 'claim-uid', 'resourceVersion': '7'},
            'spec': {'resources': {'requests': {'storage': request}}},
            'status': {'capacity': {'storage': capacity}}}


class Standalone(unittest.TestCase):
    def test_bad_other_instance_does_not_block_healthy_claim_but_blocks_partial_writeback(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d); rendered=root/'rendered'; rendered.mkdir()
            good=rendered/'boss'; good.mkdir()
            bad=rendered/'boss-playground'; bad.mkdir()
            (good/'claims.yaml').write_text(DOCUMENT)
            (bad/'claims.yaml').write_text(DOCUMENT.replace('namespace: boss','namespace: boss-playground').replace('20Gi','unread'))
            with mock.patch.object(MODULE.subprocess,'run',return_value=subprocess.CompletedProcess([],0,json.dumps(live()),'')) as reader:
                rows=MODULE.stage(good,'kubectl',rendered=rendered)
            self.assertEqual(reader.call_count,1)
            self.assertEqual(rows[0]['capacity_state'],'read')
            self.assertTrue(rows[0]['drift'])
            self.assertFalse(rows[0]['affected_claims_complete'])
            self.assertEqual(rows[0]['rendered_roster_refusals'][0]['instance'],'boss-playground')
            api=mock.Mock()
            result=MODULE.writeback(rows[0],'converge',api)
            self.assertEqual(result['state'],'recording_debt')
            api.assert_not_called()
            with self.assertRaises(ValueError):
                MODULE.inventory_all(rendered)

    def test_multiline_quoted_scalar_cannot_forge_a_floor(self):
        for quote in ('"', "'"):
            text=DOCUMENT.replace('  name: files', '  annotations:\n    note: '+quote+'x\n    boss.algedonic.dev/storage-declaration: floor\n    #'+quote+'\n  name: files')
            with self.subTest(quote=quote), tempfile.TemporaryDirectory() as d:
                (Path(d)/'claim.yaml').write_text(text)
                with self.assertRaisesRegex(ValueError, 'quoted scalar'):
                    MODULE.inventory(Path(d))

    def test_single_line_quoted_notes_keep_exact_policy_and_source_bytes(self):
        for note in ('"plain"', "'it''s fine'", r'"escaped \" quote"', '!!str "plain"', '&note "plain"'):
            text=DOCUMENT.replace('  name: files','  annotations:\n    note: '+note+' # comment\n  name: files')
            with self.subTest(note=note), tempfile.TemporaryDirectory() as d:
                path=Path(d)/'claim.yaml'; path.write_text(text)
                rows=MODULE.inventory(Path(d))
                self.assertEqual(rows[0]['storage_policy'],'exact')
                self.assertEqual(path.read_text(),text)

    def test_block_scalar_cannot_forge_a_floor_annotation(self):
        for prefix in ('|', '!!str |', '&text |'):
            text=DOCUMENT.replace('  name: files', '  annotations:\n    note: '+prefix+'\n      boss.algedonic.dev/storage-declaration: floor\n  name: files')
            with self.subTest(prefix=prefix), tempfile.TemporaryDirectory() as d:
                (Path(d)/'claim.yaml').write_text(text)
                with self.assertRaisesRegex(ValueError, 'block scalar'):
                    MODULE.inventory(Path(d))

    def test_commented_separator_keeps_unrelated_document_bytes(self):
        ordinary='kind: ConfigMap\ndata: {keep: yes}\n'
        for separator in ('--- # note\n', '---\t# note\r\n'):
            with self.subTest(separator=separator), tempfile.TemporaryDirectory() as d:
                path=Path(d)/'claim.yaml'; path.write_text(DOCUMENT+separator+ordinary)
                rows=[dict(row,absent=False) for row in MODULE.inventory(Path(d))]
                MODULE.isolate_claims(Path(d),rows)
                self.assertEqual(path.read_text(),ordinary)
                self.assertEqual(len(rows),1)

    def test_partial_isolation_cannot_create_an_unmatched_claim(self):
        with tempfile.TemporaryDirectory() as d:
            directory=Path(d); staged=directory/'standalone-new'; staged.mkdir()
            (staged/'boss--files.yaml').write_text(DOCUMENT)
            with mock.patch.object(MODULE.subprocess,'run') as writer:
                result=MODULE.create_new(directory,'kubectl','/manifests',[])
            writer.assert_not_called()
            self.assertEqual(result[0]['create_outcome']['state'],'refused')
            self.assertIn('receipt',result[0]['create_outcome']['reason'])

    def test_stage_refusal_names_source_and_known_identity(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d)/'claim.yaml').write_text(DOCUMENT)
            rows=MODULE.hold_all_claims(Path(d),'fixture refusal')
            self.assertEqual(rows[0]['namespace'],'boss')
            self.assertEqual(rows[0]['claim'],'files')
            self.assertEqual(rows[0]['held_back_document']['file'],'infra/cluster/manifests/claim.yaml')

    def test_rendered_shared_declaration_names_every_affected_claim(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)
            for ns in ('boss','boss-playground'):
                directory=root/ns; directory.mkdir()
                (directory/'claims.yaml').write_text(DOCUMENT.replace('namespace: boss','namespace: '+ns))
            rows=MODULE.inventory_all(root)
            expected=[{'namespace':ns,'claim':'files'} for ns in ('boss','boss-playground')]
            self.assertEqual(len(rows),2)
            for row in rows:
                self.assertEqual(row['affected_claims'],expected)
            calls=[]
            def api(method,path,body=None):
                calls.append(body); return 503,{}
            MODULE.writeback(dict(rows[1],live='40Gi'),'converge',api)
            self.assertIn('boss/files',calls[0]['title'])
            self.assertIn('boss-playground/files',calls[0]['title'])

    def assert_floor_roster(self, directory):
        import ast
        tree=ast.parse((ROOT/'infra/forge/dev-work-volume.py').read_text())
        targets=[ast.literal_eval(node.value) for node in tree.body
                 if isinstance(node,ast.Assign) and any(isinstance(name,ast.Name) and name.id=='TARGET' for name in node.targets)]
        self.assertEqual(len(targets),1)
        floors={(row['namespace'],row['claim']) for row in MODULE.inventory(directory) if row['storage_policy']=='floor'}
        self.assertEqual(floors,{targets[0]},'floor claims must have the declared growth door')

    def test_floor_roster_equals_the_existing_growth_door_target(self):
        self.assert_floor_roster(ROOT/'infra/cluster/manifests')

    def test_floor_roster_pin_rejects_a_new_unregistered_floor(self):
        with tempfile.TemporaryDirectory() as d:
            directory=Path(d)
            (directory/'boss-dev.yaml').write_bytes((ROOT/'infra/cluster/manifests/boss-dev.yaml').read_bytes())
            (directory/'unauthorized.yaml').write_text(DOCUMENT.replace('  name: files',
                '  annotations:\n    boss.algedonic.dev/storage-declaration: floor\n  name: files'))
            with self.assertRaisesRegex(AssertionError,'boss.*files'):
                self.assert_floor_roster(directory)

    def test_inventory_ranges_isolate_two_claims_with_distinct_indentation(self):
        other='# ordinary\nkind: ConfigMap\ndata: {untouched: yes}\n'
        second=DOCUMENT.replace('name: files','name: backups').replace('\n  ','\n    ')
        with tempfile.TemporaryDirectory() as d:
            path=Path(d)/'claims.yaml'
            path.write_text(other+'---\n'+DOCUMENT+'---\n'+other+'---\n'+second)
            def read(args,**_):
                return subprocess.CompletedProcess(args,0,json.dumps(live(name=args[3])), '')
            with mock.patch.object(MODULE.subprocess,'run',side_effect=read) as reader:
                rows=MODULE.stage(Path(d),'kubectl')
            self.assertEqual(reader.call_count,2)
            MODULE.isolate_claims(Path(d),rows)
            self.assertEqual(path.read_text(),other+'---\n'+other)
            self.assertEqual((Path(d)/'standalone-existing/boss--files.yaml').read_text(),DOCUMENT)
            self.assertEqual((Path(d)/'standalone-existing/boss--backups.yaml').read_text(),second)

    def test_isolation_preserves_nonclaim_and_audit_crlf_bytes(self):
        other=b'# ordinary\r\nkind: ConfigMap\r\ndata: {untouched: yes}\r\n'
        claim=DOCUMENT.replace('\n','\r\n').encode()
        with tempfile.TemporaryDirectory() as d:
            path=Path(d)/'claims.yaml'; path.write_bytes(other+b'---\r\n'+claim)
            rows=[dict(row,absent=False) for row in MODULE.inventory(Path(d))]
            MODULE.isolate_claims(Path(d),rows)
            self.assertEqual(path.read_bytes(),other)
            self.assertEqual((Path(d)/'standalone-existing/boss--files.yaml').read_bytes(),claim)

    def test_existing_claim_receipt_never_rewrites_the_audit_document(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/'claim.yaml'; p.write_text(DOCUMENT)
            receipt=MODULE.preserve(MODULE.inventory(Path(d))[0],live('60Gi','40Gi'))
            self.assertEqual(p.read_text(),DOCUMENT)
            self.assertNotIn('staged_request',receipt)

    def test_ambiguous_original_namespace_binding_refuses_before_read_or_staging(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d); original=root/'original'; staged=root/'staged'
            original.mkdir(); staged.mkdir()
            (original/'claims.yaml').write_text(DOCUMENT+'---\n'+DOCUMENT.replace('namespace: boss','namespace: playground').replace('20Gi','30Gi'))
            path=staged/'claims.yaml'; path.write_text(DOCUMENT)
            with mock.patch.object(MODULE.subprocess,'run',return_value=subprocess.CompletedProcess([],0,json.dumps(live()),'')) as reader:
                with self.assertRaisesRegex(ValueError,'ambiguous original'):
                    MODULE.stage(staged,'kubectl',original)
                reader.assert_not_called()
            self.assertEqual(path.read_text(),DOCUMENT)

    def test_identity_remap_and_wrong_identity_lost_ack_receipt_never_claim_recorded(self):
        for mode in ('before_compare','lost_ack_receipt'):
            with self.subTest(mode=mode):
                store={}; receipt={}; writes=[]
                def api(method,path,body=None):
                    if path=='/api/jobs':
                        store.update(copy.deepcopy(body)); return 201,{'id':store['id']}
                    if method=='GET' and '/metadata/conditional/' in path:
                        return 200,copy.deepcopy(receipt)
                    if method=='GET': return 200,copy.deepcopy(store)
                    if mode=='before_compare':
                        store['metadata']['pvc_tree_writeback']={'namespace':'foreign','claim':'different'}
                    for key,expected in body['expected'].items():
                        present=key in store['metadata']
                        if present!=expected['present'] or (present and store['metadata'][key]!=expected['value']):
                            return 409,{'error':'changed'}
                    store['metadata'].update(copy.deepcopy(body['patch'])); writes.append(body)
                    receipt.update(contract_version=1,job_id=store['id'],actor='fixture',command=copy.deepcopy(body),
                                   applied_at='2026-10-27T18:00:00Z',event_id='c3be2819-8fa6-4787-8afb-98bf67dce955',job=copy.deepcopy(store))
                    if mode=='lost_ack_receipt':
                        receipt['job']['metadata']['pvc_tree_writeback']={'namespace':'foreign','claim':'different'}
                        raise OSError('reply lost after accepted conditional command')
                    return 200,copy.deepcopy(receipt)
                api.actor='fixture'
                result=MODULE.writeback({'namespace':'boss','claim':'files','declared':'20Gi','live':'40Gi','live_bytes':40*1073741824},'converge',api)
                self.assertEqual(result['state'],'recording_debt',result)
                self.assertEqual(len(writes),0 if mode=='before_compare' else 1)

    def test_jobs_transport_preserves_non_utf8_and_timeout_bytes_before_parsing(self):
        import base64
        for timed_out in (False, True):
            api = MODULE.curl_api('http://fixture.invalid', 'fixture', '')
            def curl(args, **kwargs):
                Path(args[args.index('-o')+1]).write_bytes(b'partial\xff')
                if timed_out:
                    raise subprocess.TimeoutExpired(args, 25, output=b'partial', stderr=b'error\xff')
                return subprocess.CompletedProcess(args, 0, b'200', b'error\xff')
            with mock.patch.object(MODULE.subprocess, 'run', side_effect=curl):
                if timed_out:
                    with self.assertRaises(OSError): api('POST', '/api/jobs', {})
                else:
                    code, _ = api('POST', '/api/jobs', {})
                    self.assertEqual(code, 200)
            self.assertEqual(len(api.records),1)
            self.assertEqual(base64.b64decode(api.records[0]['response_base64']), b'partial\xff')
            self.assertEqual(base64.b64decode(api.records[0]['stderr_base64']), b'error\xff')
            self.assertEqual(api.records[0]['timed_out'],timed_out)

    def test_invalid_claim_identity_and_prebound_runtime_metadata_refuse(self):
        for doc in (DOCUMENT.replace('name: files', 'name: --server'),
                    DOCUMENT.replace('namespace: boss', 'namespace: ../boss'),
                    DOCUMENT.replace('  name: files', '  uid: stale-uid\n  name: files')):
            with tempfile.TemporaryDirectory() as d:
                (Path(d) / 'claim.yaml').write_text(doc)
                with self.assertRaises(ValueError): MODULE.inventory(Path(d))

    def test_plain_text_conflict_is_preserved_and_can_be_reconciled(self):
        with tempfile.TemporaryDirectory() as d:
            script = Path(d) / 'curl'
            script.write_text('''#!/usr/bin/env python3
import sys
from pathlib import Path
a=sys.argv[1:]
Path(a[a.index('-o')+1]).write_text('conditional row changed')
print('409',end='')
''')
            script.chmod(0o700)
            import os
            with mock.patch.dict(os.environ, PATH=d + ':' + os.environ['PATH']):
                api = MODULE.curl_api('http://fixture.invalid', 'fixture', '')
                code, _ = api('POST', '/api/jobs/fixture/metadata/conditional', {})
            self.assertEqual(code, 409)
            self.assertEqual(api.records[0]['response'], 'conditional row changed')

    def test_http_success_without_matching_conditional_receipt_is_not_recorded(self):
        state = {}
        def api(method, path, body=None):
            if path == '/api/jobs':
                state.update(body)
                return 201, {'id':state['id']}
            if method == 'GET': return 200, state
            return 200, {}
        row = {'namespace':'boss', 'claim':'files', 'declared':'20Gi', 'live':'40Gi', 'live_bytes':40*1073741824}
        self.assertEqual(MODULE.writeback(row, 'converge', api)['state'], 'recording_debt')

    def test_writeback_reconciles_lost_ack_and_preserves_a_concurrent_observation(self):
        store = {}
        calls = []
        lost = [True]
        race = [True]
        lost_conditional = [True]
        conditional_receipt = {}
        def api(method, path, body=None):
            calls.append((method, path, copy.deepcopy(body)))
            if method == 'POST' and path == '/api/jobs':
                if not store:
                    store.update(body, status='open')
                    if lost.pop():
                        raise OSError('reply lost after committed create')
                if body['title'] != store['title']:
                    return 409, {'error':'same stable identity has its first immutable envelope'}
                return 200, {'id': store['id'], 'already_admitted': True}
            if method == 'GET' and '/metadata/conditional/' in path:
                return 200, copy.deepcopy(conditional_receipt)
            if method == 'GET': return 200, copy.deepcopy(store)
            self.assertTrue(path.endswith('/metadata/conditional'))
            if race:
                race.pop()
                store['metadata']['pvc_tree_writeback_latest'] = {'observations':{'expand':{'live_bytes':60*1073741824}}}
            for key, expected in body['expected'].items():
                current = store['metadata'].get(key)
                if expected['present'] != (key in store['metadata']) or (expected['present'] and expected['value'] != current):
                    return 409, {'error': 'changed'}
            store['metadata'].update(body['patch'])
            conditional_receipt.update(contract_version=1, job_id=store['id'], actor='fixture',
                                       command=copy.deepcopy(body), applied_at='2026-10-27T17:00:00Z',
                                       event_id='daac394a-c1b2-445d-9d04-ff1868312e67', job=copy.deepcopy(store))
            if lost_conditional:
                lost_conditional.pop()
                raise OSError('reply lost after committed conditional command')
            return 200, copy.deepcopy(conditional_receipt)
        api.actor = 'fixture'
        row = {'namespace':'boss', 'claim':'files', 'declared':'20Gi', 'declared_bytes':20*1073741824,
               'file':'infra/cluster/manifests/boss.yaml', 'line':245,
               'live':'40Gi', 'live_bytes':40*1073741824, 'requested':'40Gi', 'requested_bytes':40*1073741824,
               'uid':'claim-uid', 'resource_version':'7', 'drift':True}
        first = MODULE.writeback(row, 'converge', api)
        self.assertEqual(first['state'], 'recorded')
        id_before = store['id']
        second = MODULE.writeback(dict(row, live='60Gi', live_bytes=60*1073741824), 'expand', api)
        self.assertEqual(second['state'], 'recorded')
        self.assertEqual(store['id'], id_before)
        self.assertIn('declares 20Gi, live is 40Gi',store['title'])
        self.assertEqual(set(store['metadata']['pvc_tree_writeback_latest']['observations']), {'converge', 'expand'})
        self.assertEqual(sum(method=='GET' and '/metadata/conditional/' in path for method,path,_ in calls),1)
        store['status'] = 'closed'
        self.assertEqual(MODULE.writeback(row, 'converge', api)['state'], 'recording_debt')

    def test_actual_runner_holds_an_existing_claim_and_records_live_drift(self):
        source = (ROOT / 'infra/forge/cluster-deploy-runner.sh').read_text()
        apply = re.search(r'^apply_instance\(\) \{\n.*?^\}', source, re.M | re.S).group()
        with tempfile.TemporaryDirectory() as d:
            root = Path(d); stage = root / 'boss'; stage.mkdir()
            (stage / 'boss.yaml').write_text(DOCUMENT.replace('name: files', 'name: boss-files'))
            adapter = root / 'kubectl'
            adapter.write_text('''#!/bin/bash
if [ "$1" = get ]; then cat "$FIXTURE_LIVE"; exit 0; fi
if [ "$1" = apply ]; then
  if grep -q '^kind: PersistentVolumeClaim' "$FIXTURE_STAGE/boss.yaml"; then
    echo 'PVC reached ordinary apply' >&2; exit 1
  fi
  exit 0
fi
exit 2
''')
            adapter.chmod(0o700)
            (root / 'live.json').write_text(json.dumps(live(name='boss-files')))
            script = root / 'run.sh'
            script.write_text('set -eu\n. "$REPO/infra/run-summary.sh"\n. "$REPO/infra/forge/cluster-deploy-lib.sh"\napply_namespaces() { :; }\n' + apply + '\napply_instance boss\n')
            import os
            env = dict(os.environ, REPO=str(ROOT), APPLY_DIR=str(root), K=str(adapter), KM=str(adapter),
                       KAPPLY=str(adapter), CONVERGE_STATE=str(root), HEAD_FULL='1'*40, LAST='none',
                       FIXTURE_LIVE=str(root / 'live.json'), FIXTURE_STAGE=str(stage), BOSS_RUN_SUMMARY_FILE=str(root / 'summary.json'),
                       JOBS_API='', BOSS_JOBS_URL='', BOSS_SOR_ENV=str(root / 'absent-sor.env'))
            result = subprocess.run(['bash', str(script)], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((stage / 'standalone-existing/boss--boss-files.yaml').read_text(),DOCUMENT.replace('name: files','name: boss-files'))
            summary = json.loads((root / 'summary.json').read_text())
            self.assertEqual(summary['standalone_pvc'][0]['declared'], '20Gi')
            self.assertEqual(summary['standalone_pvc'][0]['live'], '40Gi')
            self.assertEqual(summary['pvc_tree_writeback']['outcomes'][0]['state'], 'recording_debt')

    def test_live_growth_preserves_other_documents_and_original(self):
        before = '# untouched\nkind: ConfigMap\ndata: {storage: 99Gi}\n---\n' + DOCUMENT
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / 'claims.yaml'
            path.write_text(before)
            rows = MODULE.inventory(Path(d))
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]['line'], 13)
            receipt = MODULE.preserve(rows[0], live())
            self.assertEqual(receipt['declared'], '20Gi')
            self.assertEqual(receipt['live'], '40Gi')
            self.assertTrue(receipt['drift'])
            self.assertEqual(path.read_text(),before)

    def test_inflight_request_is_observed_without_rewriting_the_declaration(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / 'claim.yaml'; p.write_text(DOCUMENT)
            receipt = MODULE.preserve(MODULE.inventory(Path(d))[0], live('60Gi', '40Gi'))
            self.assertEqual(receipt['requested'], '60Gi')
            self.assertEqual(receipt['live'], '40Gi')
            self.assertTrue(receipt['drift'])
            self.assertEqual(p.read_text(),DOCUMENT)

    def test_unread_malformed_or_wrong_identity_changes_nothing(self):
        for value in ({}, live(capacity='garbage'), live(name='other')):
            with self.subTest(value=value), tempfile.TemporaryDirectory() as d:
                p = Path(d) / 'claim.yaml'; p.write_text(DOCUMENT)
                with self.assertRaises(ValueError):
                    MODULE.preserve(MODULE.inventory(Path(d))[0], value)
                self.assertEqual(p.read_text(), DOCUMENT)

    def test_new_claim_and_equal_capacity_are_unchanged(self):
        for value in (None, live('20Gi', '20Gi')):
            with tempfile.TemporaryDirectory() as d:
                p = Path(d) / 'claim.yaml'; p.write_text(DOCUMENT)
                MODULE.preserve(MODULE.inventory(Path(d))[0], value)
                if value is None:
                    self.assertEqual(p.read_text(), DOCUMENT)
                else:
                    self.assertIn('storage: 20Gi', p.read_text())

    def test_declared_floor_does_not_create_a_writeback_above_floor(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/'claim.yaml'
            p.write_text(DOCUMENT.replace('  name: files', '  annotations:\n    boss.algedonic.dev/storage-declaration: floor\n  name: files'))
            row=MODULE.inventory(Path(d))[0]
            result=MODULE.preserve(row,live('64Gi','64Gi'))
            self.assertEqual(row['storage_policy'],'floor')
            self.assertFalse(result['drift'])
        declared=MODULE.inventory(ROOT/'infra/cluster/manifests')
        dev=[r for r in declared if (r['namespace'],r['claim'])==('boss-dev','boss-dev-work')]
        self.assertEqual(len(dev),1)
        self.assertEqual(dev[0]['storage_policy'],'floor')

    def test_absent_or_null_capacity_preserves_unknown_not_a_writeback(self):
        for status in (None, {}, {'phase':'Pending'}, {'capacity':None}):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as d:
                p=Path(d)/'claim.yaml'; p.write_text(DOCUMENT)
                value=live(); value['status']=status
                result=MODULE.preserve(MODULE.inventory(Path(d))[0],value)
                self.assertFalse(result['drift'])
                self.assertEqual(result['capacity_state'],'unread')
                self.assertEqual(p.read_text(),DOCUMENT)

    def test_new_writeback_title_names_both_observed_sizes(self):
        requests=[]
        def api(method,path,body=None):
            requests.append((method,path,body))
            return 503,{}
        MODULE.writeback({'namespace':'boss','claim':'boss-files','declared':'20Gi','live':'40Gi'},'converge',api)
        self.assertIn('declares 20Gi, live is 40Gi',requests[0][2]['title'])

    def test_expansion_plan_and_effect_use_the_same_floor_policy(self):
        source=(ROOT/'infra/forge/expand-instance-volume.sh').read_text()
        predicates=re.findall(r"--argjson want \"\$WANT_B\" '([^']+)'",source)
        self.assertEqual(len(predicates),2)
        for expression in predicates:
            for policy,expected in [('exact',0),('floor',1)]:
                answer=subprocess.run(['jq','-e','--argjson','want','64',expression],
                    input=json.dumps({'declared_bytes':40,'storage_policy':policy}),text=True,capture_output=True)
                self.assertEqual(answer.returncode,expected,(policy,answer.stdout,answer.stderr))

    def test_duplicate_claim_refuses_but_namespaces_are_distinct(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / 'claim.yaml'; p.write_text(DOCUMENT + '---\n' + DOCUMENT)
            with self.assertRaises(ValueError): MODULE.inventory(Path(d))
            p.write_text(DOCUMENT + '---\n' + DOCUMENT.replace('namespace: boss', 'namespace: playground'))
            self.assertEqual(len(MODULE.inventory(Path(d))), 2)

    def test_exact_quantity_comparison_and_invalid_values(self):
        self.assertEqual(MODULE.quantity('1.5Gi'), MODULE.quantity('1536Mi'))
        self.assertEqual(MODULE.quantity('1G'), 1000000000)
        for value in ('', '-1Gi', 'NaN', '2junk', '0'):
            with self.assertRaises(ValueError): MODULE.quantity(value)

    def test_shared_integer_units_agree_with_the_existing_expansion_reader(self):
        source = (ROOT / 'infra/forge/expand-instance-volume.sh').read_text()
        expression = source.split("JQ_BYTES='", 1)[1].split("'\n", 1)[0]
        for unit in ('', 'Ki', 'Mi', 'Gi', 'Ti', 'Pi', 'k', 'M', 'G', 'T', 'P'):
            value = '3' + unit
            answer = subprocess.run(['jq', '-n', '--arg', 'quantity', value,
                                     expression + ' $quantity | bytes'],
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(answer.returncode, 0, answer.stderr)
            self.assertEqual(MODULE.quantity(value), json.loads(answer.stdout))


if __name__ == '__main__':
    unittest.main()
