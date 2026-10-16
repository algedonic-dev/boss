"""4196: closed originals are observed without editing their proof history."""
import copy
import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

SOURCE = Path(sys.argv.pop(1))
spec = importlib.util.spec_from_file_location("observation", SOURCE / "infra/forge/proof-observation.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class Controls(unittest.TestCase):
    def fixture(self, name="sponsorship-v1"):
        contract = m.observers.contract(name)
        car = {"id":contract["car"],"kind":"ship-a-change","status":"closed","partition":"real","simulated":False,
               "opened_at":"2026-10-15T08:00:00Z","metadata":{"branch":contract["branch"],"merge_ref":"a"*40,"merged":"true",
                 "proof_expect":"original broader claim","proof_attempt":{"stdout":"original attempt\n"}},
               "steps":[{"id":"33333333-3333-4333-8333-333333333333","job_id":contract["car"],"spec_slug":"proven","status":"completed",
                         "metadata":{"proof":"original complete proof bytes\n","verified":"original broader claim"}}]}
        row={"id":"11111111-1111-4111-8111-111111111111","kind":contract["kind"],"status":"closed","partition":"real","simulated":False,
             "opened_at":"2026-10-15T10:00:00Z","metadata":{},"steps":[]}
        def read(path):
            return json.dumps(car if path.startswith('/api/jobs/') else {"data":[row],"total":1,"limit":100,"offset":0,"full":True}).encode()
        def git(*args):
            if args[0]=='show':return "1760520000"  # 2025, fixed positive epoch preceding synthetic rows
            return "a"*40
        return car,row,read,git

    def test_closed_proven_original_is_observed_without_any_write_or_claim_substitution(self):
        car,row,read,git=self.fixture(); before=copy.deepcopy(car)
        code,result=m.observe("sponsorship-v1",read,git,lambda:m.e.instant("2026-10-15T11:00:00Z"))
        self.assertEqual(code,0); self.assertEqual(result['observation'],'seen')
        self.assertEqual(car,before)
        self.assertEqual(result['original']['proof_expect'],'original broader claim')
        self.assertEqual(result['contract']['body']['claim'],m.observers.contract('sponsorship-v1')['claim'])
        self.assertEqual(result['witnesses'],[row]); self.assertEqual(len(result['contract']['sha256']),64)
        self.assertEqual(result['retention']['query_pages'],'references only; complete selected witness packets retained')

    def test_bad_original_bindings_and_native_provenance_refuse(self):
        for field,value in [('id','22222222-2222-4222-8222-222222222222'),('partition','shadow'),('simulated',True)]:
            car,row,read,git=self.fixture();car[field]=value
            with self.subTest(field=field):
                code,result=m.observe('sponsorship-v1',read,git,lambda:m.e.instant('2026-10-15T11:00:00Z'))
                self.assertEqual(code,75);self.assertEqual(result['observation'],'unknown')
        for key,value in [('branch','foreign'),('merge_ref','not-a-head'),('merged','false'),('unlanded','true'),('disproved','true'),('abandoned','true')]:
            car,row,read,git=self.fixture();car['metadata'][key]=value
            self.assertEqual(m.observe('sponsorship-v1',read,git,lambda:m.e.instant('2026-10-15T11:00:00Z'))[0],75,key)
        car,row,read,git=self.fixture();row['simulated']=True
        self.assertEqual(m.observe('sponsorship-v1',read,git,lambda:m.e.instant('2026-10-15T11:00:00Z'))[0],75)

    def test_unavailable_coverage_is_unknown_and_unregistered_input_never_reads(self):
        car,row,read,git=self.fixture();row['opened_at']='2020-01-01T00:00:00Z'
        code,result=m.observe('sponsorship-v1',read,git,lambda:m.e.instant('2026-10-15T11:00:00Z'))
        self.assertEqual(code,75);self.assertEqual(result['observation'],'unknown')
        for name in ['../x','sponsorship-v1;true','unknown-v1','', 'SPONSORSHIP']:
            def forbidden(_):raise AssertionError('unregistered input performed a read')
            self.assertEqual(m.observe(name,forbidden,git,lambda:0)[0],75)

    def test_required_whole_witness_overflow_refuses_without_cropping_to_a_pass(self):
        car,row,read,git=self.fixture();row['metadata']['large']='x'*m.MAX_OUTPUT
        code,result=m.observe('sponsorship-v1',read,git,lambda:m.e.instant('2026-10-15T11:00:00Z'))
        self.assertEqual(code,75);self.assertEqual(result['observation'],'unknown')
        self.assertIn('whole',result['why']);self.assertLess(len(json.dumps(result).encode()),m.MAX_OUTPUT)

    def test_wrong_claim_car_schema_or_verdict_cannot_replace_the_envelope(self):
        car,row,read,git=self.fixture()
        original_run=m.observers.run
        for key,value in [('claim','consent was copied'),('car',row['id']),('schema',True),('observation','proven'),('original',{}),('kind','other')]:
            def changed(*args):
                code,result=original_run(*args)
                return code,{**result,key:value}
            with patch.object(m.observers,'run',side_effect=changed):
                code,result=m.observe('sponsorship-v1',read,git,lambda:m.e.instant('2026-10-15T11:00:00Z'))
                self.assertEqual(code,75,key);self.assertEqual(result['observation'],'unknown')

    def test_oversized_failure_is_explicitly_unconserved_never_a_truncated_pass(self):
        def failed(_):raise m.e.Unknown('original diagnostic '+('x'*m.MAX_OUTPUT))
        _,_,_,git=self.fixture()
        code,result=m.observe('sponsorship-v1',failed,git,lambda:0)
        self.assertEqual(code,75);self.assertLess(len(m.encoded(result)),m.MAX_OUTPUT)
        self.assertFalse(result['diagnostic']['complete'])
        self.assertGreater(result['diagnostic']['bytes'],m.MAX_OUTPUT)

    def timed_out_read(self, stdout, stderr, large_original=False, arrival_timeout=10):
        car,_,read,git=self.fixture()
        if large_original:car['metadata']['proof_expect']='original broader claim '+('x'*m.MAX_OUTPUT)
        before=copy.deepcopy(car)
        with tempfile.TemporaryDirectory(prefix='boss-observation-timeout-') as directory:
            child=Path(directory)/'child.py'
            # Markers are in the file, never argv: exception command text
            # cannot accidentally masquerade as captured original output.
            child.write_text('import sys,threading\n' +
                             'sys.stdout.buffer.write('+repr(stdout)+');sys.stdout.buffer.flush()\n' +
                             'sys.stderr.buffer.write('+repr(stderr)+');sys.stderr.buffer.flush()\n' +
                             'threading.Event().wait()\n')
            def timeout(path):
                if path.startswith('/api/jobs/'):return read(path)
                child_process=subprocess.Popen([sys.executable,'-I','-B',str(child)],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
                try:
                    # communicate() is retryable after TimeoutExpired and
                    # retains already captured bytes. Drain both pipes until
                    # the originals themselves prove readiness; this also
                    # handles streams larger than a pipe without deadlock.
                    arrival_deadline=time.monotonic()+arrival_timeout
                    arrival_samples=0
                    while True:
                        remaining=arrival_deadline-time.monotonic()
                        self.assertGreater(remaining,0,'fixture arrival watchdog expired before both original streams; not observer data loss')
                        try:
                            child_process.communicate(timeout=min(0.1,remaining))
                        except subprocess.TimeoutExpired as arrival:
                            arrival_samples+=1
                            self.assertTrue(stdout.startswith(arrival.output or b''),'fixture emitted unexpected stdout during arrival')
                            self.assertTrue(stderr.startswith(arrival.stderr or b''),'fixture emitted unexpected stderr during arrival')
                            if arrival.output==stdout and arrival.stderr==stderr:break
                        else:self.fail('fixture exited before its captured-output timeout control')
                    # Only this post-readiness real timeout is supplied to
                    # observe(). Startup samples are a separate watchdog,
                    # never an assumption about interpreter scheduling.
                    try:
                        child_process.communicate(timeout=0.5)
                    except subprocess.TimeoutExpired as error:
                        self.assertEqual(error.output,stdout);self.assertEqual(error.stderr,stderr)
                        print('timeout-fixture-control',json.dumps({'arrival_samples':arrival_samples,
                              'captured_stdout_bytes':len(error.output),'captured_stderr_bytes':len(error.stderr),
                              'post_readiness_timeout_seconds':error.timeout,'actual_timeout':True}))
                        raise
                    self.fail('ready local child did not time out')
                finally:
                    child_process.kill()
                    child_process.communicate()
            code,result=m.observe('sponsorship-v1',timeout,git,lambda:m.e.instant('2026-10-15T11:00:00Z'))
        self.assertEqual(code,75);self.assertEqual(result['observation'],'unknown')
        self.assertEqual(car,before)
        self.assertNotIn('witnesses',result);self.assertNotIn('matches',result)
        self.assertLessEqual(len(m.encoded(result))+1,m.MAX_OUTPUT)
        return result

    def test_real_timeout_retains_exact_partial_streams_and_deadline_facts(self):
        stdout=b'original stdout before deadline\xff\x00\n';stderr=b'original stderr before deadline\x80\n'
        result=self.timed_out_read(stdout,stderr)
        diagnostic=result['diagnostic']
        self.assertEqual(diagnostic['kind'],'subprocess-timeout')
        self.assertEqual(diagnostic['timeout_seconds'],0.5)
        self.assertTrue(diagnostic['complete'])
        self.assertFalse(diagnostic['process_output_complete'])
        for name,raw in [('stdout',stdout),('stderr',stderr)]:
            stream=diagnostic['streams'][name]
            self.assertEqual(base64.b64decode(stream['base64'],validate=True),raw)
            self.assertEqual(stream['bytes'],len(raw))
            self.assertEqual(stream['sha256'],hashlib.sha256(raw).hexdigest())
            self.assertTrue(stream['retained_complete'])

    def test_real_timeout_overflow_keeps_actual_stream_counts_hashes_and_incompleteness(self):
        stdout=b'oversized stdout\xff'*5000;stderr=b'oversized stderr\x80'*5000
        result=self.timed_out_read(stdout,stderr)
        diagnostic=result['diagnostic']
        self.assertFalse(diagnostic['complete'])
        self.assertFalse(diagnostic['process_output_complete'])
        for name,raw in [('stdout',stdout),('stderr',stderr)]:
            stream=diagnostic['streams'][name]
            self.assertEqual(stream['bytes'],len(raw))
            self.assertEqual(stream['sha256'],hashlib.sha256(raw).hexdigest())
            self.assertFalse(stream['retained_complete'])
            self.assertNotIn('base64',stream)
        self.assertNotIn('complete diagnostic',result['retention']['failure'])

    def test_small_timeout_streams_fit_even_when_original_metadata_does_not(self):
        stdout=b'small original stdout\xff';stderr=b'small original stderr\x80'
        result=self.timed_out_read(stdout,stderr,large_original=True)
        self.assertTrue(result['diagnostic']['complete'])
        for name,raw in [('stdout',stdout),('stderr',stderr)]:
            self.assertEqual(base64.b64decode(result['diagnostic']['streams'][name]['base64']),raw)

    def test_delayed_fixture_arrival_does_not_preempt_the_captured_output_control(self):
        real_popen=subprocess.Popen
        with tempfile.TemporaryDirectory(prefix='boss-observation-arrival-') as directory:
            launcher=Path(directory)/'arrival.py'
            launcher.write_text('import os,sys,time\ntime.sleep(0.8)\nos.execv(sys.argv[1],sys.argv[1:])\n')
            def delayed(argv,**kwargs):
                if isinstance(argv,list) and argv[-1].endswith('/child.py'):
                    return real_popen([sys.executable,'-I','-B',str(launcher),*argv],**kwargs)
                return real_popen(argv,**kwargs)
            with patch.object(subprocess,'Popen',side_effect=delayed):
                stdout=b'delayed stdout\xff';stderr=b'delayed stderr\x80'
                result=self.timed_out_read(stdout,stderr)
        for name,raw in [('stdout',stdout),('stderr',stderr)]:
            self.assertEqual(base64.b64decode(result['diagnostic']['streams'][name]['base64']),raw)

    def test_absent_fixture_arrival_fails_its_watchdog_not_observer_conservation(self):
        real_popen=subprocess.Popen
        with tempfile.TemporaryDirectory(prefix='boss-observation-no-arrival-') as directory:
            launcher=Path(directory)/'absent.py'
            launcher.write_text('import threading\nthreading.Event().wait()\n')
            def absent(argv,**kwargs):
                return real_popen([sys.executable,'-I','-B',str(launcher)],**kwargs)
            with patch.object(subprocess,'Popen',side_effect=absent):
                with self.assertRaisesRegex(AssertionError,'fixture arrival watchdog expired'):
                    self.timed_out_read(b'expected stdout',b'expected stderr',arrival_timeout=0.1)


if __name__=='__main__':unittest.main()
