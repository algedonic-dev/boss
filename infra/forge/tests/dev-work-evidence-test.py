"""Complete bounded planner evidence through the existing file record door."""
import base64
import gzip
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import contextlib
import io
from unittest import mock
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
# The owning Rust pin passes the actual subject path, so file_input_index
# sees the production dependency rather than only this Python harness.
SUBJECT = Path(sys.argv.pop(1)) if len(sys.argv)>1 and sys.argv[1].endswith('/dev-work-evidence.py') else ROOT/'infra/forge/dev-work-evidence.py'
SPEC = importlib.util.spec_from_file_location('evidence', SUBJECT)
E = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(E)
REQUEST = '7fb17ef5-1f8e-4d27-a373-fc8f85b4b67d'


def fixture():
    # Same empty stdout/951361-byte stderr envelope shape as the measured
    # request, with valid synthetic originals (never claimed native).
    source = b'synthetic original\x00\n' * 200
    record = {'argv':['get','pods','-n','boss-dev'], 'exit':0}
    for name, raw in [('stdout',source),('stderr',b'')]:
        record.update({name+'_b64':base64.b64encode(raw).decode(),name+'_bytes':len(raw),
                       name+'_sha256':hashlib.sha256(raw).hexdigest()})
    record['synthetic_padding']=''
    record['synthetic_padding']='x'*(5235422-len(json.dumps([record]).encode()))
    raw=json.dumps([record]).encode()
    assert len(raw)==5235422
    envelope={'evidence_encoding':'gzip+base64','complete':True,'request_id':REQUEST,
              'original_bytes':len(raw),'original_sha256':hashlib.sha256(raw).hexdigest(),
              'data':base64.b64encode(gzip.compress(raw,mtime=0)).decode()}
    body=json.dumps(envelope).encode()
    return body+b' '*(951361-len(body)),[record]


class Delivery(unittest.TestCase):
    def test_whole_ops_carrier_accepts_failed_diagnostics_without_claiming_a_plan(self):
        raw,_=fixture();blobs,put=self.memory();ref=E.deliver(raw,REQUEST,put,'synthetic prerequisite refusal')
        stderr=(json.dumps(ref)+'\ndev-work-volume: CANNOT ANSWER / REFUSED — synthetic prerequisite refusal\n').encode()
        def stream(body):return {'base64':base64.b64encode(body).decode(),'bytes':len(body),'retained_bytes':len(body),'sha256':E.sha(body),'complete':True}
        packet={'id':REQUEST,'kind':'ops-request','status':'closed','simulated':False,
                'metadata':{'host':'forge','verb':'plan-dev-work-volume-expansion','args':['64Gi']},
                'steps':[{'spec_slug':'execute','status':'completed','metadata':{'disposition':'answered','exit_code':'78','streams':{'stdout':stream(b''),'stderr':stream(stderr)}}}]}
        reference,carrier=E.from_packet(packet,REQUEST)
        self.assertEqual(reference,ref)
        self.assertEqual(carrier['exit'],78)
        self.assertFalse(carrier['plan_success'])
        self.assertEqual(base64.b64decode(carrier['stderr_b64']),stderr)
        spec=importlib.util.spec_from_file_location('planner',ROOT/'infra/forge/dev-work-volume.py')
        planner=importlib.util.module_from_spec(spec);spec.loader.exec_module(planner)
        with tempfile.TemporaryDirectory() as temporary:
            directory=Path(temporary);packet_file=directory/'packet.json'
            packet_file.write_text(json.dumps(packet))
            for key,body in blobs.items():(directory/key).write_bytes(body)
            output=io.StringIO()
            with contextlib.redirect_stdout(output):
                planner.main(['--decode-evidence',str(packet_file),str(directory),REQUEST])
            result=json.loads(output.getvalue())
            self.assertEqual(result['carrier']['exit'],78)
            self.assertFalse(result['carrier']['plan_success'])
        import copy
        for case in ('partial','hash','count','exit','simulation','identity','multiple'):
            bad=copy.deepcopy(packet);meta=bad['steps'][0]['metadata']
            if case=='partial':meta['streams']['stderr']['complete']=False
            if case=='hash':meta['streams']['stderr']['sha256']='0'*64
            if case=='count':meta['streams']['stderr']['retained_bytes']-=1
            if case=='exit':del meta['exit_code']
            if case=='simulation':bad['simulated']=True
            if case=='identity':bad['id']='00000000-0000-4000-8000-000000000000'
            if case=='multiple':meta['streams']['stderr']=stream(stderr+b'{}\n')
            with self.subTest(case=case),self.assertRaises(ValueError):E.from_packet(bad,REQUEST)
        bad=copy.deepcopy(packet);bad['steps'][0]['metadata']['exit_code']='0'
        with self.assertRaises(ValueError):E.from_packet(bad,REQUEST)
        successful=copy.deepcopy(packet);meta=successful['steps'][0]['metadata'];meta['exit_code']='0'
        proposal={'purpose':'dev-work-volume','target_bytes':68719476736}
        stdout=(json.dumps(proposal,sort_keys=True,indent=2)+'\n').encode()
        positive=dict(ref,failure=None)
        stderr=(json.dumps(positive)+'\nplan-sha256: '+E.sha(stdout)+'\n').encode()
        meta['streams']={'stdout':stream(stdout),'stderr':stream(stderr)}
        _,carrier=E.from_packet(successful,REQUEST)
        self.assertTrue(carrier['plan_success'])
        self.assertEqual(carrier['plan'],proposal)
        meta['streams']['stderr']=stream(stderr.replace(E.sha(stdout).encode(),b'0'*64))
        with self.assertRaises(ValueError):E.from_packet(successful,REQUEST)

    def test_supported_offline_consumer_returns_whole_records_or_no_output(self):
        spec=importlib.util.spec_from_file_location('planner',ROOT/'infra/forge/dev-work-volume.py')
        planner=importlib.util.module_from_spec(spec);spec.loader.exec_module(planner)
        raw,records=fixture();blobs,put=self.memory();ref=E.deliver(raw,REQUEST,put)
        with tempfile.TemporaryDirectory() as temporary:
            directory=Path(temporary);reference=directory/'reference.json'
            reference.write_text(json.dumps(ref))
            for key,body in blobs.items():(directory/key).write_bytes(body)
            output=io.StringIO()
            with contextlib.redirect_stdout(output):planner.main(['--decode-evidence',str(reference),str(directory),REQUEST])
            result=json.loads(output.getvalue())
            self.assertEqual(result['records'],records)
            self.assertEqual(result['carrier']['state'],'reference-only; ops carrier not verified')
            self.assertFalse(result['carrier']['plan_success'])
            key=ref['parts'][0]['file_id'];(directory/key).write_bytes(blobs[key][:-1])
            output=io.StringIO()
            with contextlib.redirect_stdout(output),self.assertRaises(ValueError):
                planner.main(['--decode-evidence',str(reference),str(directory),REQUEST])
            self.assertEqual(output.getvalue(),'')

    def test_offline_consumer_bounds_file_reads_and_refuses_symlinks(self):
        key='00000000-0000-4000-8000-000000000000'
        with tempfile.TemporaryDirectory() as temporary:
            directory=Path(temporary);path=directory/key
            path.write_bytes(b'x'*(E.PART_BYTES+1))
            with self.assertRaises(ValueError):E.read_part(directory,key)
            path.unlink();path.symlink_to(directory/'unrelated')
            (directory/'unrelated').write_bytes(b'not an evidence part')
            with self.assertRaises(ValueError):E.read_part(directory,key)
            path.unlink();path.write_bytes(b'original')
            self.assertEqual(E.read_part(directory,key),b'original')

    def test_main_keeps_prerequisite_refusal_and_never_prints_plan_on_carrier_failure(self):
        spec=importlib.util.spec_from_file_location('planner',ROOT/'infra/forge/dev-work-volume.py')
        planner=importlib.util.module_from_spec(spec);spec.loader.exec_module(planner)
        observed=[]
        class Native:
            def __init__(self,*args,**kwargs):pass
            def read(self):raise ValueError('synthetic original prerequisite refusal')
            def patch_issued(self):return False
            def evidence(self,failure=None):
                observed.append(failure)
                raise ValueError('synthetic carrier refused')
        output=io.StringIO()
        with mock.patch.object(planner,'Native',Native),mock.patch.dict(planner.os.environ,{'BOSS_DEV_WORK_KUBECTL':'unused-stub'}),contextlib.redirect_stdout(output):
            with self.assertRaisesRegex(ValueError,'complete evidence unavailable.*prerequisite refusal.*carrier refused'):
                planner.main(['--plan','64Gi'])
        self.assertEqual(output.getvalue(),'')
        self.assertEqual(observed,['synthetic original prerequisite refusal'])

    def test_real_producer_emits_only_small_reference_for_measured_envelope(self):
        spec=importlib.util.spec_from_file_location('planner',ROOT/'infra/forge/dev-work-volume.py')
        planner=importlib.util.module_from_spec(spec);spec.loader.exec_module(planner)
        native=planner.Native(['false'])
        raw,records=fixture(); native.evidence_bytes=lambda:raw
        blobs,put=self.memory(); output=io.StringIO()
        def record(body,request,failure):return E.deliver(body,request,put,failure)
        with contextlib.redirect_stderr(output):
            native.evidence(record)
        reference=json.loads(output.getvalue())
        self.assertLess(len(output.getvalue().encode()),90000)
        self.assertEqual(E.restore(reference,REQUEST,blobs.__getitem__),records)

    def memory(self):
        blobs={}
        def put(raw):
            key=f'00000000-0000-4000-8000-{len(blobs):012d}'
            blobs[key]=raw
            return key
        return blobs,put

    def test_measured_shape_round_trips_complete_originals_and_refusal_under_stream_cap(self):
        raw,records=fixture()
        self.assertEqual(len(raw),951361)
        blobs,put=self.memory()
        ref=E.deliver(raw,REQUEST,put,failure='synthetic prerequisite refused')
        self.assertLess(len(json.dumps(ref).encode()),90000)
        self.assertEqual(E.restore(ref,REQUEST,blobs.__getitem__),records)
        self.assertEqual(ref['failure'],'synthetic prerequisite refused')
        self.assertTrue(ref['complete'])

    def test_changed_missing_reordered_and_replayed_parts_refuse(self):
        raw,_=fixture(); blobs,put=self.memory()
        ref=E.deliver(raw,REQUEST,put)
        with self.assertRaises(ValueError):E.restore(ref,'00000000-0000-4000-8000-000000000000',blobs.__getitem__)
        key=ref['parts'][0]['file_id']; kept=blobs[key]
        blobs[key]=kept+b'x'
        with self.assertRaises(ValueError):E.restore(ref,REQUEST,blobs.__getitem__)
        blobs[key]=kept[:-1]
        with self.assertRaises(ValueError):E.restore(ref,REQUEST,blobs.__getitem__)
        with self.assertRaises(ValueError):E.restore(ref,REQUEST,lambda _:None)

    def test_multiple_parts_keep_order_and_hashes(self):
        raw,records=fixture();raw+=b' '*(2*E.PART_BYTES+3-len(raw))
        blobs,put=self.memory();ref=E.deliver(raw,REQUEST,put)
        self.assertEqual(len(ref['parts']),3)
        self.assertEqual(E.restore(ref,REQUEST,blobs.__getitem__),records)
        ref['parts'][0],ref['parts'][1]=ref['parts'][1],ref['parts'][0]
        with self.assertRaises(ValueError):E.restore(ref,REQUEST,blobs.__getitem__)

    def test_existing_uploader_verifies_ack_and_retains_native_failure_streams(self):
        part=b'original evidence\x00\n'
        key='00000000-0000-4000-8000-000000000000'
        good=f'as file {key}, sha256 {E.sha(part)} confirmed byte-identical'.encode()
        for code,stdout in [(0,good),(0,b'ack only'),(1,good)]:
            with tempfile.TemporaryDirectory() as temporary:
                directory=Path(temporary)
                result=subprocess.CompletedProcess([],code,stdout,b'original diagnostic\n')
                with mock.patch.object(E.subprocess,'run',return_value=result) as call:
                    if code==0 and stdout==good:
                        self.assertEqual(E.publish(part,REQUEST,directory),key)
                    else:
                        with self.assertRaises(ValueError):E.publish(part,REQUEST,directory)
                self.assertEqual(call.call_args.args[0][:3],['/usr/local/bin/boss','attach',REQUEST])
                self.assertEqual((directory/(E.sha(part)+'.native-evidence')).read_bytes(),part)
                self.assertEqual((directory/(E.sha(part)+'.native-evidence.stderr')).read_bytes(),b'original diagnostic\n')
        with tempfile.TemporaryDirectory() as temporary:
            directory=Path(temporary)
            timeout=subprocess.TimeoutExpired('attach',60,output=b'partial ack',stderr=b'timed out\n')
            with mock.patch.object(E.subprocess,'run',side_effect=timeout),self.assertRaises(ValueError):
                E.publish(part,REQUEST,directory)
            self.assertEqual((directory/(E.sha(part)+'.native-evidence.stderr')).read_bytes(),b'timed out\n')

    def test_long_refusal_original_is_in_attachment_not_replaced_by_its_short_reference(self):
        raw,records=fixture();value=json.loads(raw)
        failure='synthetic refusal detail '*400
        value['failure']=failure
        raw=json.dumps(value).encode()
        blobs,put=self.memory();ref=E.deliver(raw,REQUEST,put,failure)
        self.assertLess(len(ref['failure']),4000)
        decoded=E.restore(ref,REQUEST,blobs.__getitem__,include_failure=True)
        self.assertEqual(decoded,{'request_id':REQUEST,'failure':failure,'records':records})

    def test_over_bound_originals_are_kept_before_the_refusal(self):
        # Review b9075eb9 advisory 3: after a patch a roster can outgrow the
        # projection. The refusal stands, but the only copy of the originals
        # is written first and named, never dropped.
        oversized=b'x'*(E.MAX_ENVELOPE+1)
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)/'evidence'
            with self.assertRaisesRegex(ValueError,'exceeds bounded original carrier; originals retained in '):
                E.record(oversized,REQUEST,root=root)
            kept=list(root.glob(REQUEST+'-*/envelope.json'))
            self.assertEqual(len(kept),1)
            self.assertEqual(kept[0].read_bytes(),oversized)

    def test_the_carrier_states_no_effect_it_cannot_know(self):
        # The planner alone knows whether a patch was issued (31d76e37).
        with self.assertRaises(ValueError) as refused:
            E.deliver(fixture()[0],REQUEST,lambda part:(_ for _ in ()).throw(OSError('down')))
        self.assertNotIn('effect',str(refused.exception))

    def test_interrupted_upload_and_overflow_never_return_complete_reference(self):
        raw,_=fixture()
        with self.assertRaises(ValueError):E.deliver(raw,REQUEST,lambda _:None)
        with self.assertRaises(ValueError):E.deliver(raw,REQUEST,lambda _: (_ for _ in ()).throw(OSError('interrupted')))
        blobs,put=self.memory()
        with self.assertRaises(ValueError):E.deliver(b'x'*(E.MAX_ENVELOPE+1),REQUEST,put)
        self.assertEqual(blobs,{})

    def test_partial_multiple_malformed_compression_and_inner_changed_bytes_refuse(self):
        raw,_=fixture(); envelope=json.loads(raw)
        for change in ('partial','multiple','compression','hash','stream','missing-exit','gzip-members','decoded-overflow'):
            bad=dict(envelope)
            if change=='partial':bad['complete']=False
            if change=='compression':bad['data']=base64.b64encode(b'not gzip').decode()
            if change=='hash':bad['original_sha256']='0'*64
            if change=='gzip-members':bad['data']=base64.b64encode(base64.b64decode(bad['data'])+gzip.compress(b'extra')).decode()
            if change=='decoded-overflow':bad['original_bytes']=100
            if change in ('stream','missing-exit'):
                records=json.loads(gzip.decompress(base64.b64decode(bad['data'])))
                if change=='stream':records[0]['stdout_sha256']='0'*64
                else:del records[0]['exit']
                original=json.dumps(records).encode()
                bad.update(original_bytes=len(original),original_sha256=hashlib.sha256(original).hexdigest(),data=base64.b64encode(gzip.compress(original)).decode())
            body=json.dumps(bad).encode()+(b'{}' if change=='multiple' else b'')
            blobs,put=self.memory()
            with self.subTest(change=change),self.assertRaises(ValueError):
                ref=E.deliver(body,REQUEST,put)
                E.restore(ref,REQUEST,blobs.__getitem__)


if __name__=='__main__':unittest.main()
