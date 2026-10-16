"""Synthetic private collection controls: never contact a broker or provider."""
import importlib.util
import hashlib
import unittest
import os
import contextlib
import io
import re
import tempfile
import subprocess
import sys
from unittest.mock import patch
from pathlib import Path
spec = importlib.util.spec_from_file_location('collector', Path(__file__).with_name('private-collector.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

class Collector(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.synthetic = tempfile.TemporaryDirectory()
        root = Path(cls.synthetic.name)
        cls.ca = root / 'public.crt'
        # Offline generated fixture only; never read or print the synthetic key.
        subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048',
                        '-keyout', str(root / 'synthetic.key'), '-out', str(cls.ca),
                        '-days', '1', '-subj', '/CN=synthetic.invalid', '-nodes',
                        '-addext', 'basicConstraints=critical,CA:TRUE'],
                       check=True, capture_output=True)

    @classmethod
    def tearDownClass(cls):
        cls.synthetic.cleanup()

    def test_fixed_public_trust_ignores_environment_keylogging_and_root_selection(self):
        # The owning runner uses -I: a separate NONisolated interpreter is needed
        # to reproduce create_default_context's inherited SSLKEYLOGFILE behavior.
        code = '''import importlib.util, pathlib, ssl, sys
spec=importlib.util.spec_from_file_location('collector',sys.argv[1])
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
ca=pathlib.Path(sys.argv[2]).read_text()
client=module.FixedTransport({'deadline_seconds':120},{},b'synthetic-token',ca.encode())
assert client.context.keylog_filename is None, 'environment enabled TLS key logging'
assert client.context.verify_mode == ssl.CERT_REQUIRED
assert client.context.check_hostname is True
assert client.context.get_ca_certs(binary_form=True) == [ssl.PEM_cert_to_DER_cert(ca)]
assert not pathlib.Path(sys.argv[3]).exists(), 'TLS key log was opened'
print('fixed-public-trust; hostname-required; session-keylogging-disabled')
'''
        with tempfile.TemporaryDirectory() as temp:
            keylog = Path(temp) / 'synthetic-keylog'
            env = {**os.environ, 'SSLKEYLOGFILE': str(keylog),
                   'SSL_CERT_FILE': str(Path(temp) / 'absent-public-ca'),
                   'SSL_CERT_DIR': str(Path(temp) / 'absent-public-roots')}
            result = subprocess.run([sys.executable, '-c', code,
                                     str(Path(__file__).with_name('private-collector.py')),
                                     str(self.ca), str(keylog)], env=env,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn('session-keylogging-disabled', result.stdout)
            self.assertFalse(keylog.exists())

    def test_helper_interpreter_is_isolated_before_loading_modules(self):
        source = Path(__file__).with_name('private-collector.py').read_bytes()
        self.assertEqual(source.splitlines()[0], b'#!/usr/bin/python3 -I')
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            helper = root / 'collector'
            helper.write_bytes(source);helper.chmod(0o555)
            (root / 'ssl.py').write_text("raise RuntimeError('SYNTHETIC ENV MODULE')\n")
            result = subprocess.run([str(helper)], env={**os.environ,
                                    'PYTHONPATH': str(root),
                                    'PYTHONINSPECT': '1', 'PYTHONSTARTUP': str(root / 'ssl.py')},
                                    input='', capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 75)
            self.assertEqual(result.stderr, '')
            self.assertEqual(module.json.loads(result.stdout),
                             {'schema':1,'status':'incomplete','code':'collection-unconfirmed'})

    def test_private_limit_is_equal_to_the_contract_and_broker_kernel(self):
        contract=module.json.loads(Path(__file__).with_name('private-collector-contract.json').read_bytes())
        kernel=Path(__file__).resolve().parents[2]/'crates/orchestrators/boss-cli/src/launch_controller/managed_cache/files.rs'
        bound=re.search(r'const CACHE_LIMIT: usize = ([0-9]+) \* ([0-9]+);',kernel.read_text())
        self.assertIsNotNone(bound,'name the broker kernel bound when its representation changes')
        self.assertEqual(module.LIMIT,int(bound.group(1))*int(bound.group(2)))
        self.assertEqual(contract['limit'],module.LIMIT)

    def test_descriptor_rejects_links_wrong_mode_owner_fifo_and_oversize(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);private=root/'auth.json';private.write_bytes(b'synthetic');private.chmod(0o600)
            fd=module.directory(temp)
            try:
                self.assertEqual(module.read_file(fd,'auth.json',os.getuid(),0o600,module.LIMIT),b'synthetic')
                for fault in ['link','hardlink','mode','owner','fifo','oversize']:
                    private.unlink();private.write_bytes(b'synthetic');private.chmod(0o600)
                    if (root/'other').exists():(root/'other').unlink()
                    owner=os.getuid()
                    if fault=='link':private.unlink();private.symlink_to('/not-a-private-cache')
                    elif fault=='hardlink':os.link(private,root/'other')
                    elif fault=='mode':private.chmod(0o644)
                    elif fault=='owner':owner+=1
                    elif fault=='fifo':private.unlink();os.mkfifo(private)
                    else:private.write_bytes(b'x'*(module.LIMIT+1))
                    with self.assertRaises((OSError,ValueError)):module.read_file(fd,'auth.json',owner,0o600,module.LIMIT)
                nested=root/'link';nested.symlink_to(root,target_is_directory=True)
                with self.assertRaises(OSError):module.directory(str(nested))
            finally:os.close(fd)

    def test_direct_tls_ignores_proxy_and_refuses_redirect_without_reading_body(self):
        identity={'run':'a','nonce':'b','pod_uid':'c','workspace_uid':'d'}
        contract={'host':'fixed.internal','port':7444,'path':'/v1/collect','deadline_seconds':120}
        with patch.object(module.http.client,'HTTPSConnection') as connection,patch.dict(os.environ,{'HTTPS_PROXY':'https://attacker.invalid'}):
            client=module.FixedTransport(contract,identity,b'synthetic-token',self.ca.read_bytes())
            connection.return_value.getresponse.return_value.status=307
            with self.assertRaises(ValueError):client('collect',b'synthetic-private')
            self.assertEqual(connection.call_args.args,('fixed.internal',7444))
            self.assertEqual(client.context.verify_mode,module.ssl.CERT_REQUIRED)
            self.assertTrue(client.context.check_hostname)
            connection.return_value.getresponse.return_value.read.assert_not_called()
            connection.return_value.close.assert_called_once()

    def test_cli_refusal_emits_only_fixed_receipt_and_no_private_exception(self):
        output=io.StringIO();errors=io.StringIO()
        with contextlib.redirect_stdout(output),contextlib.redirect_stderr(errors),patch.dict(os.environ,{},clear=True),patch.object(module.Path,'read_bytes',side_effect=OSError('PRIVATE TOKEN CACHE')):
            self.assertEqual(module.main(),75)
        self.assertEqual(errors.getvalue(),'')
        self.assertNotIn('PRIVATE',output.getvalue())
        self.assertEqual(module.json.loads(output.getvalue()),{'schema':1,'status':'incomplete','code':'collection-unconfirmed'})

    def test_unavailable_authority_refuses_before_private_read(self):
        opened=[]
        def transport(phase, body): raise ValueError('SECRET provider error')
        result=module.collect({'run':'a','nonce':'b','pod_uid':'c','workspace_uid':'d'}, transport, lambda: opened.append(True))
        self.assertEqual(opened,[])
        self.assertEqual(result['status'],'incomplete')
        self.assertNotIn('SECRET',str(result))

    def test_only_exact_authorization_then_exact_private_ack_is_complete(self):
        identity={'run':'a','nonce':'b','pod_uid':'c','workspace_uid':'d'}
        private=b'PRIVATE SYNTHETIC CACHE'
        calls=[]
        def transport(phase,body):
            calls.append((phase,body))
            if phase=='authorize':return {**identity,'schema':1,'status':'authorized'}
            return {**identity,'schema':1,'status':'retained','sha256':hashlib.sha256(body).hexdigest(),'bytes':len(body)}
        result=module.collect(identity,transport,lambda:private)
        self.assertEqual(result['status'],'complete')
        self.assertEqual(calls,[('authorize',b''),('collect',private)])
        self.assertNotIn('PRIVATE',str(result))

    def test_changed_identity_extra_fields_bad_ack_oversize_and_lost_ack_refuse(self):
        identity={'run':'a','nonce':'b','pod_uid':'c','workspace_uid':'d'}
        for fault in ['run','nonce','pod_uid','workspace_uid','extra','schema','hash','count','lost','oversize']:
            opened=[];calls=[]
            def transport(phase,body):
                calls.append(phase)
                if phase=='authorize':
                    row={**identity,'schema':1,'status':'authorized'}
                    if fault in identity:row[fault]='replacement'
                    if fault=='extra':row['provider']='SECRET'
                    if fault=='schema':row['schema']=True
                    return row
                if fault=='lost':raise OSError('SECRET')
                return {**identity,'schema':1,'status':'retained','sha256':'bad' if fault=='hash' else hashlib.sha256(body).hexdigest(),'bytes':True if fault=='count' else len(body)}
            def read():opened.append(True);return b'x'*(module.LIMIT+1) if fault=='oversize' else b'private'
            result=module.collect(identity,transport,read)
            self.assertEqual(result['status'],'incomplete',fault)
            self.assertNotIn('SECRET',str(result))
            self.assertLessEqual(calls.count('collect'),1)
            if fault in identity or fault in ('extra','schema'):self.assertEqual(opened,[])

if __name__=='__main__':unittest.main()
