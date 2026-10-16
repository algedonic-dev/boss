"""Owning controls for the immutable agent image publication record."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import subprocess
import sys
import os
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("image_witness", Path(__file__).with_name("image-witness.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ImageWitness(unittest.TestCase):
    def test_private_collector_requires_both_exact_native_image_owned_hashes(self):
        root,generation=self.fixture()
        helper=b'fixed private helper';contract=b'fixed public contract'
        for phase,raw,path in [('collector-helper',helper,'/usr/local/bin/boss-private-cache-collector'),('collector-contract',contract,'/usr/share/boss-agent/private-collector-contract.json')]:
            (root/(phase+'-source')).write_bytes(raw)
            (root/(phase+'.stdout')).write_text(hashlib.sha256(raw).hexdigest()+'  '+path+'\n')
            with (root/'native-commands.jsonl').open('a') as file:file.write(json.dumps({'phase':phase,'exit':0,'argv':['fixed-native-fixture']})+'\n')
        result=self.build(root,generation)
        self.assertEqual(result['collector_helper_sha256'],hashlib.sha256(helper).hexdigest())
        self.assertEqual(result['collector_contract_sha256'],hashlib.sha256(contract).hexdigest())
        (root/'image-witness.json').write_text(json.dumps(result));self.assertEqual(module.readiness(root),result)
        for name in ['collector-helper.stdout','collector-contract.stdout','collector-helper-source','collector-contract-source']:
            path=root/name;original=path.read_bytes();path.write_bytes(original+b'changed')
            with self.assertRaises(ValueError):self.build(root,generation)
            path.write_bytes(original)
        commands=(root/'native-commands.jsonl').read_text()
        rows=[json.loads(line) for line in commands.splitlines()];rows[-1]['exit']=False
        (root/'native-commands.jsonl').write_text('\n'.join(map(json.dumps,rows))+'\n')
        with self.assertRaises(ValueError):self.build(root,generation)
        (root/'native-commands.jsonl').write_text(commands)
        self.assertIn('COPY infra/agent/private-collector.py /usr/local/bin/boss-private-cache-collector',Path(__file__).with_name('Dockerfile').read_text())
        publisher=Path(__file__).with_name('publish-image.sh').read_text()
        for phase in ['collector-helper','collector-contract']:
            self.assertIn('native '+phase+' no ',publisher)
            self.assertIn('$raw/'+phase+'-source',publisher)

    def test_historical_witness_does_not_claim_a_private_helper(self):
        root,generation=self.fixture()
        result=self.build(root,generation)
        self.assertNotIn('collector_helper_sha256',result)
        self.assertNotIn('collector_contract_sha256',result)

    def descriptor_fixture(self):
        root,generation=self.fixture()
        manifest=json.loads((root/'manifest.json').read_bytes())
        manifest['mediaType']='application/vnd.oci.image.manifest.v1+json'
        raw=json.dumps(manifest).encode();(root/'manifest.json').write_bytes(raw);(root/'digest').write_text(module.digest(raw))
        config=json.loads((root/'image-config.json').read_bytes())
        child={'mediaType':manifest['mediaType'],'digest':module.digest(raw),'size':len(raw),'platform':{'os':'linux','architecture':'amd64'}}
        index=json.dumps({'schemaVersion':2,'mediaType':'application/vnd.oci.image.index.v1+json','manifests':[child]}).encode()
        (root/'tag.json').write_bytes(index)
        base={'Os':'linux','Architecture':'amd64','Config':config['config'],'RootFS':{'Type':'layers','Layers':config['rootfs']['diff_ids']}}
        built={**base,'Id':module.digest(index),'Descriptor':{'mediaType':'application/vnd.oci.image.index.v1+json','digest':module.digest(index),'size':len(index)},'RepoDigests':['registry.invalid/david/boss-agent-codex-exec@'+module.digest(index)]}
        pulled={**base,'Id':module.digest(raw),'Descriptor':{k:v for k,v in child.items() if k!='platform'},'RepoDigests':['registry.invalid/david/boss-agent-codex-exec@'+module.digest(raw)]}
        (root/'build-inspect.stdout').write_text(json.dumps([built]));(root/'inspect.stdout').write_text(json.dumps([pulled]))
        return root,generation

    def test_native_index_and_pulled_manifest_are_bound_to_same_verified_image(self):
        root,generation=self.descriptor_fixture()
        result=self.build(root,generation)
        self.assertEqual(result['built_config_digest'],json.loads((root/'manifest.json').read_bytes())['config']['digest'])
        self.assertEqual(result['built_image_identity'],json.loads((root/'build-inspect.stdout').read_text())[0])
        self.assertEqual(result['built_index_original'],(root/'tag.json').read_text())
        (root/'image-witness.json').write_text(json.dumps(result));self.assertEqual(module.readiness(root),result)

    def test_reused_index_publication_retains_original_binding_without_fake_build(self):
        root,generation=self.descriptor_fixture();original=self.build(root,generation)
        (root/'reused-publication.json').write_text(json.dumps(original))
        rows=(root/'native-commands.jsonl').read_text().splitlines();(root/'native-commands.jsonl').write_text('\n'.join(rows[3:])+'\n')
        (root/'tag.json').unlink();(root/'build-inspect.stdout').unlink()
        reused=self.build(root,'b'*40)
        self.assertEqual(reused['built_image_identity'],original['built_image_identity'])
        self.assertEqual(reused['built_index_original'],original['built_index_original'])
        self.assertEqual(reused['image_generation'],generation)

    def test_native_descriptor_chain_refuses_missing_ambiguous_or_changed_originals(self):
        for change in ['missing-index','index-bytes','index-size','child-digest','child-size','duplicate-platform','wrong-platform','unknown-index-type','bool-child-size','nested-index','native-id','pulled-id','pulled-size','pulled-type','native-config','native-rootfs','native-platform','missing-native-config']:
            with self.subTest(change=change):
                root,generation=self.descriptor_fixture()
                built=json.loads((root/'build-inspect.stdout').read_text())[0]
                pulled=json.loads((root/'inspect.stdout').read_text())[0]
                index=json.loads((root/'tag.json').read_bytes())
                if change=='missing-index':(root/'tag.json').unlink()
                elif change=='index-bytes':(root/'tag.json').write_bytes((root/'tag.json').read_bytes()+b' ')
                elif change=='index-size':built['Descriptor']['size']+=1
                elif change in ['child-digest','child-size','duplicate-platform','wrong-platform','unknown-index-type','bool-child-size','nested-index']:
                    if change=='child-digest':index['manifests'][0]['digest']='sha256:'+'b'*64
                    elif change=='child-size':index['manifests'][0]['size']+=1
                    elif change=='duplicate-platform':index['manifests'].append(index['manifests'][0].copy())
                    elif change=='wrong-platform':index['manifests'][0]['platform']['architecture']='arm64'
                    elif change=='unknown-index-type':index['mediaType']='unknown'
                    elif change=='bool-child-size':index['manifests'][0]['size']=True
                    else:index['manifests'][0]['mediaType']='application/vnd.oci.image.index.v1+json'
                    raw=json.dumps(index).encode();(root/'tag.json').write_bytes(raw)
                    built['Id']=module.digest(raw);built['Descriptor']['digest']=module.digest(raw);built['Descriptor']['size']=len(raw)
                elif change=='native-id':built['Id']='sha256:'+'b'*64
                elif change=='pulled-id':pulled['Id']='sha256:'+'b'*64
                elif change=='pulled-size':pulled['Descriptor']['size']+=1
                elif change=='pulled-type':pulled['Descriptor']['mediaType']='unknown'
                elif change=='native-config':built['Config']['Labels']['org.opencontainers.image.revision']='b'*40
                elif change=='native-rootfs':built['RootFS']['Layers'][0]='sha256:'+'b'*64
                elif change=='native-platform':built['Architecture']='arm64'
                else:built.pop('Config')
                (root/'build-inspect.stdout').write_text(json.dumps([built]));(root/'inspect.stdout').write_text(json.dumps([pulled]))
                with self.assertRaises(ValueError):self.build(root,generation)

    def test_failed_reader_retains_bound_index_original_without_publishing_a_witness(self):
        root,attempt,stage,generation=self.failed_fixture()
        modern,_=self.descriptor_fixture()
        for name in ['build-inspect.stdout','inspect.stdout','manifest.json','image-config.json','digest','tag.json']:
            (stage/name).write_bytes((modern/name).read_bytes())
        with patch.object(module,'trusted_path',side_effect=lambda path:path):
            result=module.failed_image_originals(attempt,generation,root/'agent-images')
        self.assertEqual(result['identity_originals']['tag.json']['original'],(stage/'tag.json').read_text())
        self.assertEqual(result['receipt']['state'],'failed')
        self.assertFalse(result['launch_ready'])
        for fault in ['missing','linked','changed']:
            if fault=='missing':(stage/'tag.json').unlink()
            elif fault=='linked':(stage/'tag.json').symlink_to(modern/'tag.json')
            else:(stage/'tag.json').unlink();(stage/'tag.json').write_bytes((modern/'tag.json').read_bytes()+b' ')
            with patch.object(module,'trusted_path',side_effect=lambda path:path),self.assertRaises(ValueError):
                module.failed_image_originals(attempt,generation,root/'agent-images')

    def failed_fixture(self):
        root,generation=self.fixture()
        attempt=root/('agent-publication.'+generation+'.ABCDEFGH');attempt.mkdir()
        stage=root/'agent-images'/('.pending.'+generation+'.IJKLMNOP')/'agent';stage.mkdir(parents=True)
        for name in ['build-inspect.stdout','inspect.stdout','manifest.json','image-config.json','digest','native-commands.jsonl']:
            (stage/name).write_bytes((root/name).read_bytes())
        rows=[json.loads(line) for line in (stage/'native-commands.jsonl').read_text().splitlines()]
        for row in rows:row['argv']=['/usr/bin/docker','--host','unix:///var/run/docker.sock',row['phase']]
        rows[0]['argv'] += ['--label','org.opencontainers.image.revision='+generation,'--tag','registry.invalid/david/boss-agent-codex-exec:'+generation]
        (stage/'native-commands.jsonl').write_text('\n'.join(map(json.dumps,rows))+'\n')
        (attempt/'stdout').write_text('complete original stdout\n');(attempt/'stderr').write_text('registry artifact differs from the native built image\n')
        (attempt/'receipt.json').write_text(json.dumps({'schema':1,'generation':generation,'state':'failed','exit':1,'stdout':str(attempt/'stdout'),'stderr':str(attempt/'stderr'),'launch_ready':False}))
        return root,attempt,stage,generation

    def test_failed_image_reader_returns_only_bound_complete_identity_originals(self):
        root,attempt,stage,generation=self.failed_fixture()
        (stage/'token.json').write_text('must not be read')
        # Root ownership is an infrastructure port unavailable to gate UID;
        # the real path guard is exercised independently below.
        with patch.object(module,'trusted_path',side_effect=lambda path:path):
            result=module.failed_image_originals(attempt,generation,root/'agent-images')
        self.assertFalse(result['launch_ready'])
        self.assertEqual(result['receipt']['state'],'failed')
        self.assertEqual(result['stage_binding'],'unique-generation-originals')
        self.assertEqual(set(result['identity_originals']),set(['build-inspect.stdout','inspect.stdout','manifest.json','image-config.json','digest','native-commands.jsonl']))
        for name,row in result['identity_originals'].items():
            self.assertEqual(row['original'],(stage/name).read_text())
            self.assertEqual(row['sha256'],hashlib.sha256((stage/name).read_bytes()).hexdigest())

    def test_failed_image_reader_refuses_ambiguous_incomplete_or_changed_bindings(self):
        for change in ['second-attempt','second-stage','missing','linked','wrong-generation','unknown-phase','nonfailed','bad-config','oversized','wrong-manifest','native-failed','native-bool','unknown-socket','malformed-label','unknown-receipt']:
            with self.subTest(change=change):
                root,attempt,stage,generation=self.failed_fixture()
                if change=='second-attempt': (root/('agent-publication.'+generation+'.QRSTUVWX')).mkdir()
                elif change=='second-stage':(root/'agent-images'/('.pending.'+generation+'.QRSTUVWX')).mkdir()
                elif change=='missing':(stage/'inspect.stdout').unlink()
                elif change=='linked':(stage/'inspect.stdout').unlink();(stage/'inspect.stdout').symlink_to(root/'inspect.stdout')
                elif change in ['wrong-generation','unknown-phase','native-failed','native-bool','unknown-socket','malformed-label']:
                    path=stage/'native-commands.jsonl';text=path.read_text()
                    substitutions={'wrong-generation':(generation,'b'*40),'unknown-phase':('"phase": "helper"','"phase": "unknown"'),'native-failed':('"exit": 0','"exit": 1'),'native-bool':('"exit": 0','"exit": false'),'unknown-socket':('unix:///var/run/docker.sock','unix:///unknown.sock'),'malformed-label':('"--label", "org.opencontainers.image.revision='+generation+'", ','')}
                    before,after=substitutions[change];path.write_text(text.replace(before,after))
                elif change in ['nonfailed','unknown-receipt']:
                    path=attempt/'receipt.json';row=json.loads(path.read_text())
                    if change=='nonfailed':row['state']='unconfirmed'
                    else:row['unknown']='must not be emitted'
                    path.write_text(json.dumps(row))
                elif change=='oversized':(stage/'inspect.stdout').write_bytes(b'x'*(1024**2+1))
                elif change=='wrong-manifest':(stage/'manifest.json').write_bytes((stage/'manifest.json').read_bytes()+b' ')
                else:(stage/'image-config.json').write_text('{}')
                with patch.object(module,'trusted_path',side_effect=lambda path:path),self.assertRaises(ValueError):
                    module.failed_image_originals(attempt,generation,root/'agent-images')

    def test_failed_image_reader_uses_actual_trusted_path_guard(self):
        root,attempt,stage,generation=self.failed_fixture()
        with self.assertRaises(ValueError):module.failed_image_originals(attempt,generation,root/'agent-images')
    def test_recipe_installs_the_publisher_executable_despite_inherited_npm_prefix(self):
        # Execute the recipe's install through real npm, substituting only the
        # offline vendor fixture and a private image filesystem (9880c699).
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); package=root/'vendor';package.mkdir()
            (package/'package.json').write_text(json.dumps({'name':'@openai/codex','version':'0.159.3','bin':{'codex':'codex'}}))
            (package/'codex').write_text('#!/bin/sh\ncase "$*" in --version) echo "codex-cli 0.159.3";; "exec --help") echo "--json --ignore-user-config --model --sandbox --output-last-message";; *) exit 94;; esac\n')
            (package/'codex').chmod(0o755)
            tools=root/'tools';tools.mkdir(); image=root/'image';image.mkdir()
            wrapper=tools/'npm'
            wrapper.write_text('#!'+sys.executable+'\nimport os,subprocess,sys\nfrom pathlib import Path\nargs=sys.argv[1:]\nargs=[os.environ["FIXTURE_VENDOR"] if x.startswith("@openai/codex@") else os.environ["FIXTURE_IMAGE"]+"/usr/local" if x=="/usr/local" else x for x in args]\nraise SystemExit(subprocess.run(["/usr/bin/npm",*args,"--offline","--no-audit","--no-fund"]).returncode)\n')
            wrapper.chmod(0o755)
            recipe=Path(__file__).with_name('Dockerfile').read_text()
            install=re.search(r'RUN (test -n.*?)(?=\nRUN --network=none)',recipe,re.S).group(1).replace('\\\n','')
            env={**os.environ,'PATH':str(tools)+':/usr/bin:/bin','HOME':str(root),'CODEX_VERSION':'0.159.3',
                 'NPM_CONFIG_PREFIX':str(image/'inherited-prefix'),'npm_config_cache':str(root/'cache'),
                 'FIXTURE_VENDOR':str(package),'FIXTURE_IMAGE':str(image)}
            installed=subprocess.run(['bash','-c',install],capture_output=True,text=True,env=env)
            self.assertEqual(installed.returncode,0,installed.stdout+installed.stderr)
            executable=image/'usr/local/bin/codex'
            self.assertTrue(executable.is_file(),'recipe inherited npm prefix; native /usr/local/bin/codex is absent')
            self.assertFalse((image/'inherited-prefix/bin/codex').exists())
            self.assertEqual(subprocess.check_output([str(executable),'--version'],text=True).strip(),'codex-cli 0.159.3')
            closure=re.search(r'&& (npm list[^>]+)> /usr/share/boss-agent/package-closure.json',recipe).group(1)
            listed=subprocess.run(['bash','-c',closure],capture_output=True,text=True,env=env)
            self.assertEqual(listed.returncode,0,listed.stderr)
            self.assertEqual(json.loads(listed.stdout)['dependencies']['@openai/codex']['version'],'0.159.3')
            self.assertIn('&& /usr/local/bin/codex --version >',recipe)
            self.assertIn('&& /usr/local/bin/codex exec --help >',recipe)
            # Exercise the publisher's exact native version/help calls against
            # a Docker port that refuses any absent image entrypoint with127.
            publisher=Path(__file__).with_name('publish-image.sh').read_text()
            probe='\n'.join(line for line in publisher.splitlines() if line.startswith('PROBE=') or line.startswith('native version ') or line.startswith('native help '))
            docker=tools/'docker'
            docker.write_text('#!'+sys.executable+'\nimport os,subprocess,sys\nfrom pathlib import Path\na=sys.argv[1:]; i=a.index("--entrypoint"); p=Path(os.environ["FIXTURE_IMAGE"]+a[i+1]); tail=a[i+3:]\nif not p.is_file():\n print("cannot stat "+a[i+1],file=sys.stderr);raise SystemExit(127)\nraise SystemExit(subprocess.run([str(p),*tail]).returncode)\n')
            docker.chmod(0o755)
            script=root/'probe.sh'
            script.write_text('set -euo pipefail\nPIN=fixed-image\nnative() { shift 2; docker "$@"; }\n'+probe+'\n')
            good=subprocess.run(['bash',str(script)],capture_output=True,text=True,env=env)
            self.assertEqual(good.returncode,0,good.stderr)
            executable.unlink()
            missing=subprocess.run(['bash',str(script)],capture_output=True,text=True,env=env)
            self.assertEqual(missing.returncode,127,missing.stderr)
            self.assertIn('cannot stat /usr/local/bin/codex',missing.stderr)

    def test_registry_transport_refuses_service_paths_before_actual_curl(self):
        received=[]
        class Receiver(BaseHTTPRequestHandler):
            def do_GET(self):
                received.append((self.path,dict(self.headers)))
                self.send_response(200);self.end_headers();self.wfile.write(b'complete-local-registry-response')
            def log_message(self,*args): pass
        server=ThreadingHTTPServer(('127.0.0.1',0),Receiver)
        thread=Thread(target=server.serve_forever,daemon=True);thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        source=Path(__file__).with_name('publish-image.sh').read_text()
        function=source.split('http_get() {',1)[1].split('\nfetch_image()',1)[0]
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); (root/'home').mkdir()
            authority='127.0.0.1:'+str(server.server_port)
            script=root/'transport.sh'
            script.write_text('set -euo pipefail\nHERE="'+str(Path(__file__).parent)+'"\nstage="'+str(root)+'"\nSCHEME=http\nIMAGE_REPO="'+authority+'/agent"\nTOOLCHAIN="'+authority+'/toolchain:tag"\nhttp_get() {'+function+'\nhttp_get "$1" "'+str(root/'body')+'" "'+str(root/'headers')+'"\n')
            url='http://'+authority
            accepted=subprocess.run(['bash',str(script),url+'/v2/agent/manifests/tag'],capture_output=True,text=True)
            self.assertEqual(accepted.returncode,0,accepted.stderr)
            self.assertEqual((root/'body').read_bytes(),b'complete-local-registry-response')
            self.assertEqual(len(received),1)
            for path in ['/api/jobs','/v2/%2e%2e/api/jobs','/v2/token?redirect=/api/jobs']:
                refused=subprocess.run(['bash',str(script),url+path],capture_output=True,text=True)
                self.assertNotEqual(refused.returncode,0,refused.stdout+refused.stderr)
                self.assertEqual(len(received),1,'service path reached curl before registry classification')
                self.assertIn('registry transport URL',refused.stderr)
            self.assertFalse(any(key.lower() in ['x-boss-user','x-boss-machine-token'] for key in received[0][1]))

    def test_registry_url_policy_has_only_named_v2_authorities(self):
        repositories=['registry.invalid:3000/owner/agent','toolchain.invalid:3000/owner/ci:tag']
        for url in ['http://registry.invalid:3000/v2/','http://registry.invalid:3000/v2/token?service=container_registry&scope=repository:owner/agent:pull','http://toolchain.invalid:3000/v2/owner/ci/blobs/sha256:abc']:
            self.assertEqual(module.registry_url(url,'http',repositories),url)
        for url in ['http://other.invalid:3000/v2/','http://registry.invalid:3000/api/jobs','http://user@registry.invalid:3000/v2/','http://registry.invalid:3000/v2/../api/jobs','http://registry.invalid:3000/v2/%2e%2e/api/jobs','http://registry.invalid:3000/v2/#fragment','https://registry.invalid:3000/v2/','http://registry.invalid:3000/v2/\\api/jobs','http://registry.invalid:3000/v2/token?redirect=/api/jobs','http://registry.invalid:3000/v2/token?service=container_registry&scope=repository:other/image:pull','http://registry.invalid:3000/v2/token?service=container_registry&scope=repository:owner/agent:push','http://registry.invalid:3000/v2/?scope=repository:owner/agent:pull']:
            with self.subTest(url=url),self.assertRaises(ValueError): module.registry_url(url,'http',repositories)

    def test_optional_publication_failure_preserves_core_and_full_originals(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root/'last-good').write_text('previous immutable witness')
            publisher = root/'publisher'
            publisher.write_text('#!/bin/bash\nprintf "full first line\\nfull final line\\n"\nexit 37\n')
            publisher.chmod(0o700)
            script = root/'test.sh'
            stage = Path(__file__).with_name('publication-stage.sh')
            script.write_text('set -euo pipefail\n. '+str(stage)+'\nrun_summary_json() { printf "%s\\n" "$2" > "'+str(root/'record')+'"; }\nagent_image_publication_stage "'+str(publisher)+'" '+('a'*40)+' "'+str(root)+'"\nprintf "core remains converged\\n"\n')
            out = subprocess.run(['bash',str(script)], capture_output=True,text=True)
            self.assertEqual(out.returncode,0,out.stderr)
            self.assertIn('core remains converged',out.stdout)
            record=json.loads((root/'record').read_text())
            self.assertEqual((record['state'],record['exit']),('failed',37))
            self.assertNotIn('image',record)
            self.assertEqual(Path(record['stdout']).read_text(),'full first line\nfull final line\n')
            self.assertEqual((root/'last-good').read_text(),'previous immutable witness')

    def test_publication_records_uncertainty_before_invoking_the_producer(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            publisher=root/'publisher'
            publisher.write_text('#!/bin/bash\npython3 -I - "'+str(root/'record')+'" <<\'CHECK\'\nimport json,sys\nfrom pathlib import Path\nrow=json.loads(Path(sys.argv[1]).read_text())\nassert row["state"]=="unconfirmed" and row["exit"] is None\nassert json.loads(Path(row["stdout"]).with_name("receipt.json").read_text())==row\nCHECK\n[ "$?" = 0 ] || exit 46\nexit 37\n')
            publisher.chmod(0o700)
            stage=Path(__file__).with_name('publication-stage.sh')
            script=root/'test.sh'
            script.write_text('set -euo pipefail\n. '+str(stage)+'\nrun_summary_json() { printf "%s\\n" "$2" > "'+str(root/'record')+'"; }\nagent_image_publication_stage "'+str(publisher)+'" '+('a'*40)+' "'+str(root)+'"\n')
            out=subprocess.run(['bash',str(script)],capture_output=True,text=True)
            self.assertEqual(out.returncode,0,out.stderr)
            self.assertEqual(json.loads((root/'record').read_text())['exit'],37)

    def test_optional_publication_accepts_exact_image_only_receipt_and_refuses_silence(self):
        for valid in [True,False]:
            with tempfile.TemporaryDirectory() as directory:
                root=Path(directory)
                row={'generation':'a'*40,'kind':'agent-image','image':'registry.invalid/image@sha256:'+'a'*64,'launch_ready':False}
                publisher=root/'publisher'
                publisher.write_text('#!/bin/bash\ncat <<\'RECEIPT\'\n'+(json.dumps(row) if valid else '')+'\nRECEIPT\n')
                publisher.chmod(0o700)
                stage=Path(__file__).with_name('publication-stage.sh')
                script=root/'test.sh'
                script.write_text('set -euo pipefail\n. '+str(stage)+'\nrun_summary_json() { printf "%s\\n" "$2" > "'+str(root/'record')+'"; }\nagent_image_publication_stage "'+str(publisher)+'" '+('a'*40)+' "'+str(root)+'"\n')
                out=subprocess.run(['bash',str(script)],capture_output=True,text=True)
                self.assertEqual(out.returncode,0,out.stderr)
                record=json.loads((root/'record').read_text())
                self.assertEqual(record['state'],'published' if valid else 'invalid-receipt')
                if valid: self.assertEqual(record['image'],row['image'])
                else: self.assertNotIn('image',record)

    def test_root_store_refuses_replaceable_ancestors_and_links(self):
        root, _ = self.fixture()
        with self.assertRaises(ValueError): module.trusted_path(root)
        self.assertEqual(module.trusted_path(Path('/')), Path('/'))
        link = root/'linked'
        link.symlink_to('/')
        with self.assertRaises(ValueError): module.trusted_path(link)

    def test_producer_shared_blob_guard_refuses_before_native_image_execution(self):
        library = Path(__file__).parents[1]/'estate/registry-manifest.sh'
        self.assertTrue(library.is_file(), 'published shared registry transport prerequisite missing')
        source = Path(__file__).with_name('publish-image.sh').read_text()
        fetch = source[source.index('fetch_image() {'):source.index('\nfetch_image "${TOOLCHAIN')]
        for corrupt in [False, True]:
            root, _ = self.fixture()
            manifest = json.loads((root/'manifest.json').read_text())
            (root/'header').write_text('')
            out = root/'fetched';out.mkdir()
            objects = root/'objects';objects.mkdir()
            for name, descriptor in module.descriptors(manifest):
                (objects/descriptor['digest']).write_bytes((root/name).read_bytes())
            if corrupt:
                (objects/manifest['layers'][0]['digest']).write_bytes(b'contradictory bytes')
            script = root/'producer.sh'
            script.write_text('set -euo pipefail\nSCHEME=http; HERE="'+str(Path(__file__).parent)+'"\n. "'+str(library)+'"\nrefuse() { echo "$*" >&2; exit 43; }\nrefuse_http() { echo "$*" >&2; exit 44; }\ndigest_of() { printf "sha256:"; sha256sum "$1" | cut -d " " -f 1; }\nhttp_get() { [ "$TOKEN_HDR" = "@'+str(root/'header')+'" ] || exit 45; cp "'+str(objects)+'/${1##*/}" "$2"; printf "HTTP/1.1 200\\n" > "$3"; printf 200; }\nregistry_manifest() { cp "'+str(root/'manifest.json')+'" "$3/manifest.json"; cp "'+str(root/'digest')+'" "$3/digest"; printf "%s\\n" "@'+str(root/'header')+'" > "$3/header-path"; }\n'+fetch+'\nfetch_image registry.invalid/image tag "'+str(out)+'"\nprintf native-executed > "'+str(root/'native-effect')+'"\n')
            result = subprocess.run(['bash',str(script)],capture_output=True,text=True)
            if corrupt:
                self.assertEqual(result.returncode,43,result.stderr)
                self.assertFalse((root/'native-effect').exists())
                self.assertEqual((out/'layer-0.blob').read_bytes(),b'contradictory bytes')
            else:
                self.assertEqual(result.returncode,0,result.stderr)
                self.assertTrue((root/'native-effect').exists())

    def test_publication_refuses_before_optional_bytes_consume_core_disk_floor(self):
        root, _ = self.fixture()
        module.publication_capacity(root, 32 * 1024**3)
        with self.assertRaises(ValueError): module.publication_capacity(root, 19 * 1024**3)
        with (root/'retained.blob').open('wb') as handle: handle.truncate(64 * 1024**3)
        with self.assertRaises(ValueError): module.publication_capacity(root, 100 * 1024**3)

    def test_verified_layer_staging_can_leave_only_after_native_retention_ack(self):
        root, generation = self.fixture()
        module.retain_verified_blobs(root)
        (root/'layer-0.blob').unlink()
        result = self.build(root,generation)
        (root/'image-witness.json').write_text(json.dumps(result))
        self.assertEqual(module.readiness(root)['image'],result['image'])
        proof=json.loads((root/'blob-verification.json').read_text())
        proof['blobs'][1]['observed_bytes'] += 1
        (root/'blob-verification.json').write_text(json.dumps(proof))
        with self.assertRaises(ValueError): module.readiness(root)

    def test_wrong_native_retention_digest_refuses_publication(self):
        root, generation = self.fixture()
        (root/'inspect.stdout').write_text('[]')
        with self.assertRaises(ValueError): self.build(root,generation)

    def test_registry_artifact_must_equal_the_native_built_image(self):
        root,generation=self.fixture()
        (root/'build-inspect.stdout').write_text(json.dumps([{'Id':'sha256:'+'b'*64}]))
        with self.assertRaises(ValueError): self.build(root,generation)

    def test_reuse_fingerprint_changes_with_each_actual_image_input(self):
        root,_=self.fixture()
        names=['infra/agent/Dockerfile','infra/agent/agent-io.py','infra/platform/harnesses/codex-exec.toml','infra/platform/agent-controllers/codex-exec.toml','infra/agent/private-collector.py','infra/agent/private-collector-contract.json']
        for name in names:
            path=root/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_text(name)
        (root/names[2]).write_text('argv = ["codex", "exec", "--json", "--sandbox", "{sandbox}"]\n')
        original=module.image_inputs(root,'registry.invalid/toolchain@sha256:'+'a'*64)
        for name in names:
            path=root/name;before=path.read_bytes();path.write_bytes(before+b'\n# changed\n')
            self.assertNotEqual(module.image_inputs(root,'registry.invalid/toolchain@sha256:'+'a'*64),original)
            path.write_bytes(before)
        self.assertNotEqual(module.image_inputs(root,'registry.invalid/toolchain@sha256:'+'b'*64),original)

    def test_help_contract_comes_from_the_harness_registry_and_refuses_flag_prefixes(self):
        root,generation=self.fixture()
        (root/'help.stdout').write_bytes(b'--json-extra --ignore-user-config --model --sandbox --output-last-message\n')
        with self.assertRaises(ValueError): self.build(root,generation)
        names=['infra/agent/Dockerfile','infra/agent/agent-io.py','infra/platform/harnesses/codex-exec.toml','infra/platform/agent-controllers/codex-exec.toml','infra/agent/private-collector.py','infra/agent/private-collector-contract.json']
        for name in names:
            path=root/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_text(name)
        (root/names[2]).write_text('argv = ["codex", "exec", "--json", "--new-harness-flag"]\n')
        actual=module.image_inputs(root,'registry.invalid/toolchain@sha256:'+'a'*64)
        self.assertEqual(actual['harness_flags'],['--json','--new-harness-flag'])

    def test_reuse_keeps_original_image_generation_without_fictitious_build_effect(self):
        root,generation=self.fixture()
        original=self.build(root,generation)
        (root/'reused-publication.json').write_text(json.dumps(original))
        rows=[json.loads(line) for line in (root/'native-commands.jsonl').read_text().splitlines()]
        (root/'native-commands.jsonl').write_text('\n'.join(json.dumps(row) for row in rows[3:])+'\n')
        next_record=self.build(root,'b'*40)
        self.assertEqual(next_record['generation'],'b'*40)
        self.assertEqual(next_record['image_generation'],generation)
        self.assertEqual(next_record['native_commands'][0]['phase'],'pull')
        self.assertEqual(next_record['reused_publication'],original)

    def test_optional_publisher_has_a_fixed_finite_process_bound(self):
        stage=Path(__file__).with_name('publication-stage.sh').read_text()
        self.assertIn('/usr/bin/timeout --signal=TERM --kill-after=30s 1800 "$publisher"',stage)

    def test_failure_reader_conserves_full_streams_and_refuses_mismatched_paths(self):
        root,generation=self.fixture()
        (root/'stdout').write_text('first line\nlast line\n')
        (root/'stderr').write_text('actual failure\n')
        row={'schema':1,'generation':generation,'state':'failed','exit':37,
             'stdout':str(root/'stdout'),'stderr':str(root/'stderr'),'launch_ready':False}
        (root/'receipt.json').write_text(json.dumps(row))
        actual=module.attempt_record(root,generation)
        self.assertEqual(actual['stdout_original'],'first line\nlast line\n')
        self.assertEqual(actual['stderr_original'],'actual failure\n')
        row['stdout']=str(root/'version.stdout')
        (root/'receipt.json').write_text(json.dumps(row))
        with self.assertRaises(ValueError): module.attempt_record(root,generation)

    def test_native_argv_options_cannot_change_the_recording_parser(self):
        root, _ = self.fixture()
        (root/'native-commands.jsonl').unlink()
        out = subprocess.run([sys.executable, '-I', str(Path(__file__).with_name('image-witness.py')), 'command', str(root), '--phase', 'version', '--exit-code', '0', '--', 'run', '--version'], capture_output=True)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(json.loads((root/'native-commands.jsonl').read_text())['argv'][-2:], ['run', '--version'])

    def test_shared_transport_header_contract_uses_a_file_reference(self):
        root,_=self.fixture()
        source=Path(__file__).with_name('publish-image.sh').read_text()
        self.assertIn('. "$HERE/../lib/secret-header.sh"',source)
        library=Path(__file__).parents[1]/'lib/secret-header.sh'
        script=root/'header.sh'
        script.write_text('set -euo pipefail\nunset RUNTIME_DIRECTORY\nexport TMPDIR="'+str(root)+'"\n. "'+str(library)+'"\nsecret_header TOKEN_HDR "X-Synthetic-Fixture: probe"\nprintf "%s\\n" "$TOKEN_HDR"\ncat "${TOKEN_HDR#@}"\n')
        result=subprocess.run(['bash',str(script)],capture_output=True,text=True)
        self.assertEqual(result.returncode,0,result.stderr)
        reference,line=result.stdout.splitlines()
        self.assertTrue(reference.startswith('@'+str(root)+'/'))
        self.assertEqual(line,'X-Synthetic-Fixture: probe')
        self.assertFalse(Path(reference[1:]).exists())

    def test_downloaded_vendor_executes_only_in_network_none_build_stages(self):
        recipe=Path(__file__).with_name('Dockerfile').read_text()
        self.assertIn('RUN --network=none',recipe)
        download=recipe.split('RUN test -n')[1].split('RUN --network=none')[0]
        self.assertIn('--ignore-scripts',download)
        self.assertNotIn('codex --version',download)
        self.assertNotIn('codex exec --help',download)
        self.assertNotIn('apt-get',recipe)
        publisher=Path(__file__).with_name('publish-image.sh').read_text()
        self.assertIn('environment+=("DOCKER_BUILDKIT=1")',publisher)

    def test_recipe_takes_its_version_from_the_published_controller_row(self):
        recipe = Path(__file__).with_name('Dockerfile').read_text()
        self.assertIn('ARG CODEX_VERSION', recipe)
        self.assertIn('@openai/codex@${CODEX_VERSION}', recipe)
        self.assertNotIn('@openai/codex@0.159.3', recipe)
    def fixture(self):
        root = Path(tempfile.mkdtemp(prefix="agent-image-witness-"))
        self.addCleanup(lambda: __import__('shutil').rmtree(root))
        generation = 'a' * 40
        helper = b'image-owned helper\n'
        config = json.dumps({'architecture': 'amd64', 'os': 'linux', 'config': {'Labels': {'org.opencontainers.image.revision': generation}},'rootfs':{'type':'layers','diff_ids':['sha256:'+'c'*64]}}).encode()
        layer = b'complete layer bytes, verified before image execution'
        def descriptor(raw):
            return {'digest': 'sha256:' + hashlib.sha256(raw).hexdigest(), 'size': len(raw)}
        manifest = json.dumps({'schemaVersion': 2, 'config': descriptor(config), 'layers': [descriptor(layer)]}).encode()
        for name, raw in [('manifest.json', manifest), ('image-config.json', config), ('layer-0.blob', layer), ('helper-source', helper), ('version.stdout', b'codex-cli 0.159.3\n'), ('help.stdout', b'--json --ignore-user-config --model --sandbox --output-last-message\n'), ('package-closure.stdout', b'{"dependencies":{"@openai/codex":{"version":"0.159.3"}}}'), ('helper.stdout', hashlib.sha256(helper).hexdigest().encode() + b'  /usr/local/bin/boss-agent-io\n')]:
            (root/name).write_bytes(raw)
        (root/'digest').write_text(descriptor(manifest)['digest'])
        (root/'native-commands.jsonl').write_text('\n'.join(json.dumps({'phase': phase, 'exit': 0, 'argv': ['fixed-native-fixture']}) for phase in ['build', 'build-inspect', 'push', 'pull', 'inspect', 'version', 'help', 'package-closure', 'helper'])+'\n')
        native={'Id':descriptor(config)['digest'],'Os':'linux','Architecture':'amd64','Config':json.loads(config)['config'],'RootFS':{'Type':'layers','Layers':json.loads(config)['rootfs']['diff_ids']}}
        (root/'build-inspect.stdout').write_text(json.dumps([native]))
        (root/'inspect.stdout').write_text(json.dumps([{**native,'RepoDigests':['registry.invalid/david/boss-agent-codex-exec@'+descriptor(manifest)['digest']]}]))
        (root/'image-inputs.json').write_text(json.dumps({'schema':1,'toolchain':'registry.invalid/toolchain@sha256:'+'a'*64,'source_files':{},'fingerprint':'a'*64,'harness_flags':['--json','--ignore-user-config','--model','--sandbox','--output-last-message']}))
        return root, generation

    def build(self, root, generation):
        return module.witness(root, generation, 'registry.invalid/david/boss-agent-codex-exec', 'codex-cli 0.159.3', root/'helper-source')

    def test_complete_actual_records_form_image_only_witness(self):
        root, generation = self.fixture()
        result = self.build(root, generation)
        self.assertEqual(result['generation'], generation)
        self.assertEqual(result['image_platform'], 'linux/amd64')
        self.assertTrue(result['image'].startswith('registry.invalid/david/boss-agent-codex-exec@sha256:'))
        self.assertNotIn('credential', result)
        self.assertEqual(result['originals']['version.stdout']['bytes'], len(b'codex-cli 0.159.3\n'))

    def test_a_missing_or_failed_native_effect_cannot_publish_readiness(self):
        root, generation = self.fixture()
        (root/'native-commands.jsonl').write_text('')
        with self.assertRaises(ValueError): self.build(root, generation)

    def test_readiness_consumes_the_published_witness_and_rechecks_originals(self):
        root, generation = self.fixture()
        result = self.build(root, generation)
        (root/'image-witness.json').write_text(json.dumps(result))
        self.assertEqual(module.readiness(root)['image'], result['image'])
        (root/'help.stdout').write_bytes(b'changed retained help')
        with self.assertRaises(ValueError): module.readiness(root)
        root, generation = self.fixture()
        (root/'native-commands.jsonl').write_text((root/'native-commands.jsonl').read_text().replace('"exit": 0', '"exit": 1', 1))
        with self.assertRaises(ValueError): self.build(root, generation)

    def test_every_blob_and_manifest_is_verified_before_witness(self):
        for name in ['manifest.json', 'image-config.json', 'layer-0.blob']:
            root, generation = self.fixture()
            (root/name).write_bytes((root/name).read_bytes()+b'corrupt')
            with self.assertRaises(ValueError): self.build(root, generation)

    def test_wrong_platform_generation_version_flags_helper_or_package_refuses(self):
        changes = [('version.stdout', b'codex-cli 0.159.4\n'), ('help.stdout', b'--sandbox only\n'), ('helper.stdout', b'0'*64+b'  helper\n'), ('package-closure.stdout', b'{}')]
        for name, raw in changes:
            root, generation = self.fixture(); (root/name).write_bytes(raw)
            with self.assertRaises(ValueError): self.build(root, generation)
        root, generation = self.fixture()
        with self.assertRaises(ValueError): self.build(root, 'b'*40)

    def test_missing_or_extra_layer_and_linked_original_refuses(self):
        root, generation = self.fixture(); (root/'layer-1.blob').write_bytes(b'extra')
        with self.assertRaises(ValueError): self.build(root, generation)

    def test_source_inputs_derive_the_existing_registry_and_gate_image(self):
        root, _ = self.fixture()
        (root/'infra/platform/agent-controllers').mkdir(parents=True)
        (root/'infra/platform/harnesses').mkdir(parents=True)
        (root/'infra/gate-runner').mkdir(parents=True)
        (root/'infra/platform/agent-controllers/codex-exec.toml').write_text('required_binary_version = "codex-cli 0.159.3"\n')
        (root/'infra/platform/harnesses/codex-exec.toml').write_text('id = "codex-exec"\nargv = ["codex", "exec", "--json", "--ignore-user-config", "--model", "{model}", "--sandbox", "{sandbox}", "--output-last-message", "{last_message}", "-"]\n')
        (root/'infra/gate-runner/gate-runner.yaml').write_text('containers:\n  - name: gate\n    image: registry.invalid/david/boss-ci:approved\n')
        self.assertEqual(module.source_inputs(root), ('codex-cli 0.159.3', 'registry.invalid/david/boss-ci:approved'))
        (root/'infra/gate-runner/gate-runner.yaml').write_text('containers:\n  - name: gate\n    image: one\n  - name: gate\n    image: two\n')
        with self.assertRaises(ValueError): module.source_inputs(root)
        root, generation = self.fixture(); (root/'version.stdout').unlink(); (root/'version.stdout').symlink_to(root/'help.stdout')
        with self.assertRaises(ValueError): self.build(root, generation)


if __name__ == '__main__': unittest.main()
