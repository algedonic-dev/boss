#!/usr/bin/python3 -I
"""Image-only publication evidence. Credentials and launch readiness are separate."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import tomllib
import shutil
from urllib.parse import parse_qs, urlsplit

COLLECTOR_PHASES = ('collector-helper', 'collector-contract')
NATIVE_PUBLICATION_PHASES = ('build','build-inspect','push','pull','inspect','version','help','package-closure','helper')
INDEX_TYPES = ('application/vnd.oci.image.index.v1+json','application/vnd.docker.distribution.manifest.list.v2+json')
MANIFEST_TYPES = ('application/vnd.oci.image.manifest.v1+json','application/vnd.docker.distribution.manifest.v2+json')


def registry_url(url, scheme, repositories):
    parsed=urlsplit(url)
    authorities={repository.split('/',1)[0] for repository in repositories}
    if (scheme not in ['http','https'] or parsed.scheme!=scheme
            or parsed.netloc not in authorities or parsed.username is not None
            or parsed.fragment or re.search(r'[\s\\]',url)
            or not parsed.path.startswith('/v2/') or '%' in parsed.path
            or any(segment in ['.','..'] for segment in parsed.path.split('/'))):
        raise ValueError('registry transport URL is outside the named image authorities and /v2/ paths')
    if parsed.query:
        query=parse_qs(parsed.query,keep_blank_values=True,strict_parsing=True)
        scopes={'repository:'+repository.split('/',1)[1].rsplit(':',1)[0]+':pull'
                for repository in repositories if repository.split('/',1)[0]==parsed.netloc}
        if (parsed.path!='/v2/token' or set(query)!= {'service','scope'}
                or len(query['service'])!=1 or len(query['scope'])!=1
                or not re.fullmatch('[a-zA-Z0-9._:-]+',query['service'][0])
                or query['scope'][0] not in scopes):
            raise ValueError('registry transport URL query is not the named repository anonymous pull scope')
    return url


def source_inputs(source):
    row = tomllib.loads(regular(source/'infra/platform/agent-controllers/codex-exec.toml').decode())
    harness = tomllib.loads(regular(source/'infra/platform/harnesses/codex-exec.toml').decode())
    version = row['required_binary_version']
    if not re.fullmatch(r'codex-cli [0-9]+\.[0-9]+\.[0-9]+', version) or harness['id'] != 'codex-exec' or harness['argv'][:2] != ['codex', 'exec']:
        raise ValueError('published harness/version contract differs')
    text = regular(source/'infra/gate-runner/gate-runner.yaml').decode()
    images = re.findall(r'(?m)^([ \t]*)- name: gate\n\1  image: ([^\s]+)[ \t]*$', text)
    if len(images) != 1 or not re.fullmatch(r'[a-zA-Z0-9.-]+(?::[0-9]+)?/[a-zA-Z0-9._/-]+:[a-zA-Z0-9._-]+', images[0][1]):
        raise ValueError('no unique named gate toolchain image')
    return version, images[0][1]


def image_inputs(source, toolchain):
    names = ['infra/agent/Dockerfile','infra/agent/agent-io.py',
             'infra/agent/private-collector.py','infra/agent/private-collector-contract.json',
             'infra/platform/harnesses/codex-exec.toml',
             'infra/platform/agent-controllers/codex-exec.toml']
    if not re.fullmatch(r'[^\s@]+@sha256:[0-9a-f]{64}',toolchain):
        raise ValueError('toolchain is not the verified immutable artifact')
    harness=tomllib.loads(regular(source/'infra/platform/harnesses/codex-exec.toml').decode())
    flags=sorted({word for word in harness['argv'] if word.startswith('--')})
    if not flags or any(not re.fullmatch(r'--[a-z][a-z-]*',word) for word in flags):
        raise ValueError('harness does not declare a bounded executable flag contract')
    data={'schema':1,'toolchain':toolchain,
          'harness_flags':flags,
          'source_files':{name:hashlib.sha256(regular(source/name)).hexdigest() for name in names}}
    return {**data,'fingerprint':hashlib.sha256(json.dumps(data,sort_keys=True).encode()).hexdigest()}


def find_reuse(store, inputs):
    for candidate in sorted(store.iterdir()):
        if re.fullmatch('[0-9a-f]{40}',candidate.name) and not candidate.is_symlink():
            trusted_path(candidate)
            prior=readiness(candidate/'agent')
            if prior['image_inputs']==inputs and prior.get('reused_publication') is None:
                return prior
    return None


def publication_capacity(store, free_bytes):
    # A complete toolchain and agent are each bounded to 8 GiB. Reserve that
    # before admission; refuse instead of erasing a previous original.
    retained = 0
    entries = 0
    for entry in store.rglob('*'):
        entries += 1
        if entries > 100000 or entry.is_symlink():
            raise ValueError('publication store is outside the bounded roster')
        if entry.is_file(): retained += entry.stat().st_size
    if free_bytes < 20 * 1024**3 or retained + 16 * 1024**3 > 64 * 1024**3:
        raise ValueError('optional image publication capacity exhausted; originals retained')


def trusted_path(path):
    if not path.is_absolute() or path != path.resolve():
        raise ValueError('trusted publication path is not absolute and physical')
    for entry in [path, *path.parents]:
        info = entry.lstat()
        if entry.is_symlink() or info.st_uid != 0 or info.st_mode & 0o022:
            raise ValueError('publication path has a replaceable ancestor')
    return path


def regular(path):
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"original is not a regular file: {path.name}")
    return path.read_bytes()


def digest(raw):
    return "sha256:" + hashlib.sha256(raw).hexdigest()


def file_hash(path):
    if path.is_symlink() or not path.is_file():
        raise ValueError(f'original is not regular: {path.name}')
    hasher = hashlib.sha256()
    length = 0
    with path.open('rb') as handle:
        while chunk := handle.read(1024**2):
            hasher.update(chunk); length += len(chunk)
    return length, hasher.hexdigest()


def descriptors(manifest):
    if manifest.get('schemaVersion') != 2 or not isinstance(manifest.get('layers'), list):
        raise ValueError('unsupported complete image manifest')
    if not 0 < len(manifest['layers']) <= 128:
        raise ValueError('image layer count is outside the publication bound')
    result = [('image-config.json', manifest['config'])]
    result += [(f'layer-{i}.blob', row) for i, row in enumerate(manifest['layers'])]
    total = 0
    for name, row in result:
        size = row.get('size')
        if not re.fullmatch(r'sha256:[0-9a-f]{64}', row.get('digest', '')) or type(size) is not int or not 0 < size <= 2 * 1024**3:
            raise ValueError('invalid image blob descriptor')
        if name == 'image-config.json' and size > 1024**2:
            raise ValueError('image configuration exceeds the bound')
        total += size
    if total > 8 * 1024**3:
        raise ValueError('complete image exceeds the publication bound')
    return result


def retain_verified_blobs(root):
    manifest, _, expected = verified(root)
    records = []
    for name, descriptor in descriptors(manifest):
        length, actual = file_hash(root/name)
        records.append({'name':name, 'expected_bytes':descriptor['size'],
                        'observed_bytes':length, 'expected_digest':descriptor['digest'],
                        'observed_digest':'sha256:'+actual})
    (root/'blob-verification.json').write_text(json.dumps({'schema':1,
        'manifest_digest':expected,'blobs':records},indent=2)+'\n')


def verified(root):
    manifest_raw = regular(root/'manifest.json')
    expected = regular(root/'digest').decode().strip()
    if digest(manifest_raw) != expected:
        raise ValueError('platform manifest digest differs')
    manifest = json.loads(manifest_raw)
    rows = descriptors(manifest)
    layers = set(p.name for p in root.glob('layer-*.blob'))
    expected_layers = {n for n, _ in rows if n.startswith('layer-')}
    if layers and layers != expected_layers:
        raise ValueError('layer files differ from complete descriptor roster')
    proof = None
    if not layers:
        proof = json.loads(regular(root/'blob-verification.json'))
        expected_records = [{'name':name,'expected_bytes':row['size'],
            'observed_bytes':row['size'],'expected_digest':row['digest'],
            'observed_digest':row['digest']} for name,row in rows]
        if proof != {'schema':1,'manifest_digest':expected,'blobs':expected_records}:
            raise ValueError('retained blob verification differs from complete descriptors')
    for name, descriptor in rows:
        if name.startswith('layer-') and not layers: continue
        length, actual_hash = file_hash(root/name)
        if length != descriptor['size'] or f'sha256:{actual_hash}' != descriptor['digest']:
            raise ValueError(f'blob digest or size differs: {name}')
    config = json.loads(regular(root/'image-config.json'))
    if (config.get('os'), config.get('architecture')) != ('linux', 'amd64'):
        raise ValueError('image platform differs')
    return manifest, config, expected


def native_image_identity(native, manifest_raw, config, index_raw=None, allow_index=False):
    """Docker's Id names a descriptor on the containerd store, not its config.

    Authenticate that descriptor's complete chain, retaining the older config
    identity dialect only when it has no descriptor and matches exactly.
    """
    manifest=json.loads(manifest_raw)
    rootfs=config.get('rootfs',{})
    if not isinstance(rootfs,dict):
        raise ValueError('image root filesystem configuration is not an object')
    diff_ids=rootfs.get('diff_ids')
    if (native.get('Os'),native.get('Architecture'))!=(config.get('os'),config.get('architecture')) or not isinstance(config.get('config'),dict) or native.get('Config')!=config['config'] or rootfs.get('type')!='layers' or not isinstance(diff_ids,list) or len(diff_ids)!=len(manifest['layers']) or any(not isinstance(value,str) or not re.fullmatch(r'sha256:[0-9a-f]{64}',value) for value in diff_ids) or native.get('RootFS')!={'Type':'layers','Layers':diff_ids}:
        raise ValueError('native image configuration or root filesystem differs')
    descriptor=native.get('Descriptor')
    if descriptor is None:
        if native.get('Id')!=manifest['config']['digest']:
            raise ValueError('legacy native configuration identity differs')
        return
    if not isinstance(descriptor,dict) or native.get('Id')!=descriptor.get('digest') or type(descriptor.get('size')) is not int or not 0<descriptor['size']<=1024**2:
        raise ValueError('native descriptor identity or size differs')
    kind=descriptor.get('mediaType')
    if kind in MANIFEST_TYPES:
        if descriptor['digest']!=digest(manifest_raw) or descriptor['size']!=len(manifest_raw) or manifest.get('mediaType')!=kind:
            raise ValueError('native platform manifest descriptor differs')
        return
    if kind not in INDEX_TYPES or not allow_index or not isinstance(index_raw,bytes) or len(index_raw)>1024**2 or descriptor['digest']!=digest(index_raw) or descriptor['size']!=len(index_raw):
        raise ValueError('native index has no complete bound original')
    index=json.loads(index_raw)
    rows=index.get('manifests')
    if type(index.get('schemaVersion')) is not int or index['schemaVersion']!=2 or index.get('mediaType')!=kind or not isinstance(rows,list) or not 0<len(rows)<=128:
        raise ValueError('native index schema or roster differs')
    for row in rows:
        if not isinstance(row,dict) or row.get('mediaType') not in MANIFEST_TYPES or not isinstance(row.get('digest'),str) or not re.fullmatch(r'sha256:[0-9a-f]{64}',row['digest']) or type(row.get('size')) is not int or not 0<row['size']<=1024**2 or not isinstance(row.get('platform'),dict):
            raise ValueError('native index member descriptor is invalid')
    selected=[row for row in rows if (row['platform'].get('os'),row['platform'].get('architecture'))==('linux','amd64')]
    if len(selected)!=1 or selected[0]['digest']!=digest(manifest_raw) or selected[0]['size']!=len(manifest_raw) or selected[0]['mediaType']!=manifest.get('mediaType'):
        raise ValueError('native index has no unique bound platform manifest')


def witness(root, generation, repository, version, helper_source):
    if not re.fullmatch('[0-9a-f]{40}', generation):
        raise ValueError('generation must be an exact source commit')
    if not re.fullmatch(r'[a-zA-Z0-9.-]+(?::[0-9]+)?/[a-zA-Z0-9._/-]+', repository):
        raise ValueError('repository is not a named registry repository')
    manifest, config, image_digest = verified(root)
    commands = [json.loads(line) for line in regular(root/'native-commands.jsonl').splitlines()]
    phases = [row.get('phase') for row in commands]
    inputs=json.loads(regular(root/'image-inputs.json'))
    reuse=None
    image_generation=generation
    collector_fields = {}
    has_collector = any(phase in COLLECTOR_PHASES for phase in phases)
    expected_phases=list(NATIVE_PUBLICATION_PHASES + (COLLECTOR_PHASES if has_collector else ()))
    built_config_digest=None
    built_identity=None
    built_index=None
    if (root/'reused-publication.json').exists():
        reuse=json.loads(regular(root/'reused-publication.json'))
        if reuse.get('kind')!='agent-image' or reuse.get('image')!=f'{repository}@{image_digest}' or reuse.get('image_inputs')!=inputs or reuse.get('launch_ready') is not False:
            raise ValueError('reused immutable publication differs from exact image inputs')
        image_generation=reuse['image_generation']
        built_config_digest=reuse['built_config_digest']
        built_identity=reuse.get('built_image_identity')
        built_index=reuse.get('built_index_original')
        expected_phases=expected_phases[3:]
    if phases != expected_phases or any(type(row.get('exit')) is not int or row['exit'] != 0 or not row.get('argv') for row in commands):
        raise ValueError('native publication effects are incomplete or failed')
    if config.get('config', {}).get('Labels', {}).get('org.opencontainers.image.revision') != image_generation:
        raise ValueError('actual image generation differs')
    if reuse is None:
        built=json.loads(regular(root/'build-inspect.stdout'))
        if len(built)!=1:
            raise ValueError('native built image observation is not unique')
        built_identity=built[0]
        built_config_digest=manifest['config']['digest']
        if (root/'tag.json').exists(): built_index=regular(root/'tag.json').decode()
    if built_config_digest!=manifest['config']['digest']:
        raise ValueError('registry artifact differs from the native built image')
    if not isinstance(built_identity,dict):
        raise ValueError('original native built identity absent')
    native_image_identity(built_identity,regular(root/'manifest.json'),config,
                          built_index.encode() if isinstance(built_index,str) else None,True)
    inspection = json.loads(regular(root/'inspect.stdout'))
    pin = f'{repository}@{image_digest}'
    if len(inspection) != 1 or pin not in inspection[0].get('RepoDigests', []) or (inspection[0].get('Os'), inspection[0].get('Architecture')) != ('linux','amd64') or inspection[0].get('Config',{}).get('Labels',{}).get('org.opencontainers.image.revision') != image_generation:
        raise ValueError('native daemon retention acknowledgement differs')
    native_image_identity(inspection[0],regular(root/'manifest.json'),config)
    observed_version = regular(root/'version.stdout').decode().strip()
    if observed_version != version:
        raise ValueError('actual image binary version differs')
    help_raw = regular(root/'help.stdout')
    for flag in inputs['harness_flags']:
        if not re.search(rb'(?<![\w-])'+re.escape(flag.encode())+rb'(?![\w-])',help_raw):
            raise ValueError('actual image executable lacks a required harness flag')
    package = json.loads(regular(root/'package-closure.stdout'))
    package_version = package.get('dependencies', {}).get('@openai/codex', {}).get('version')
    if f'codex-cli {package_version}' != version:
        raise ValueError('actual package closure differs from required binary')
    helper_sha = hashlib.sha256(regular(helper_source)).hexdigest()
    if regular(root/'helper.stdout').decode().split() != [helper_sha, '/usr/local/bin/boss-agent-io']:
        raise ValueError('actual image helper differs from published source')
    if has_collector:
        for phase,path,key in [('collector-helper','/usr/local/bin/boss-private-cache-collector','collector_helper_sha256'),('collector-contract','/usr/share/boss-agent/private-collector-contract.json','collector_contract_sha256')]:
            sha = hashlib.sha256(regular(root/(phase+'-source'))).hexdigest()
            if regular(root/(phase+'.stdout')).decode().split() != [sha,path]:
                raise ValueError('actual private collector image bytes differ')
            collector_fields[key] = sha
        if reuse is not None and any(reuse.get(key) != value for key,value in collector_fields.items()):
            raise ValueError('reused collector witnesses differ')
    elif any(name.startswith('collector-') for name in inputs.get('source_files', {})) or 'infra/agent/private-collector.py' in inputs.get('source_files', {}):
        raise ValueError('collector source inputs require native collector witnesses')
    originals = {}
    for path in sorted(root.iterdir()):
        if path.is_file() and path.name not in ['image-witness.json', 'header-path']:
            length, actual_hash = file_hash(path)
            originals[path.name] = {'bytes': length, 'sha256': actual_hash}
    return {**collector_fields, 'schema': 1, 'kind': 'agent-image', 'generation': generation, 'image_generation':image_generation, 'built_config_digest':built_config_digest,'built_image_identity':built_identity,'built_index_original':built_index,'image_inputs':inputs, 'reused_publication':reuse, 'image': f'{repository}@{image_digest}',
            'image_platform': 'linux/amd64', 'binary_version': observed_version,
            'argv_help_sha256': hashlib.sha256(help_raw).hexdigest(), 'helper_sha256': helper_sha,
            'package_closure_sha256': hashlib.sha256(regular(root/'package-closure.stdout')).hexdigest(),
            'blob_descriptors': [{'name': name, **row} for name, row in descriptors(manifest)], 'originals': originals, 'native_commands': commands,
            'launch_ready': False, 'remaining': ['issuer-owned installed login generation', 'controller generation and kubectl witness', 'authorized native acceptance']}


def readiness(root):
    saved = json.loads(regular(root/'image-witness.json'))
    actual = witness(root, saved['generation'], saved['image'].split('@')[0], saved['binary_version'], root/'helper-source')
    if saved != actual:
        raise ValueError('published witness no longer matches complete originals')
    return actual


def attempt_record(root, generation):
    row=json.loads(regular(root/'receipt.json'))
    if row.get('generation')!=generation or row.get('launch_ready') is not False:
        raise ValueError('attempt receipt differs from requested generation')
    for stream in ['stdout','stderr']:
        if row.get(stream)!=str(root/stream):
            raise ValueError('attempt receipt names a different original stream')
    return {'kind':'agent-image-publication-attempt','generation':generation,
            'launch_ready':False,'receipt':row,
            'stdout_original':regular(root/'stdout').decode(),
            'stderr_original':regular(root/'stderr').decode()}


def failed_image_originals(attempt, generation, store):
    """Read one failed generation's fixed image records, never a witness.

    Legacy attempts did not name their stage. Only an unambiguous generation
    with a complete native build identity can recover that association.
    """
    if not re.fullmatch('[0-9a-f]{40}',generation):
        raise ValueError('failed original generation is not exact')
    trusted_path(attempt)
    trusted_path(store)
    attempts=list(attempt.parent.glob('agent-publication.'+generation+'.????????'))
    stages=list(store.glob('.pending.'+generation+'.????????'))
    if attempts != [attempt] or len(stages)!=1:
        raise ValueError('failed publication originals are ambiguous or absent')
    if not re.fullmatch(r'agent-publication\.'+generation+r'\.[a-zA-Z0-9]{8}',attempt.name) or not re.fullmatch(r'\.pending\.'+generation+r'\.[a-zA-Z0-9]{8}',stages[0].name):
        raise ValueError('failed original directory identity differs')
    for name in ['receipt.json','stdout','stderr']:
        trusted_path(attempt/name)
    result=attempt_record(attempt,generation)
    receipt=result['receipt']
    if set(receipt)!={'schema','generation','state','exit','stdout','stderr','launch_ready'} or type(receipt.get('schema')) is not int or receipt['schema']!=1 or receipt.get('state')!='failed' or type(receipt.get('exit')) is not int or receipt['exit']==0:
        raise ValueError('publication attempt has no failed terminal')
    stage=trusted_path(stages[0]/'agent')
    names=['build-inspect.stdout','inspect.stdout','manifest.json','image-config.json','digest','native-commands.jsonl']
    originals={}
    for name in names:
        path=trusted_path(stage/name)
        if not path.is_file() or path.stat().st_size>1024**2:
            raise ValueError('failed identity original is absent or exceeds the bound')
        raw=regular(path)
        originals[name]={'original':raw.decode(),'bytes':len(raw),'sha256':hashlib.sha256(raw).hexdigest()}
    commands=[json.loads(line) for line in originals['native-commands.jsonl']['original'].splitlines()]
    if tuple(row.get('phase') for row in commands) not in (NATIVE_PUBLICATION_PHASES, NATIVE_PUBLICATION_PHASES + COLLECTOR_PHASES) or any(type(row.get('exit')) is not int or row['exit']!=0 or not isinstance(row.get('argv'),list) or any(not isinstance(word,str) for word in row['argv']) or row['argv'][:3]!=['/usr/bin/docker','--host','unix:///var/run/docker.sock'] for row in commands):
        raise ValueError('failed originals have incomplete or unknown native commands')
    build=commands[0]['argv']
    if build[3:4]!=['build'] or build.count('--label')!=1 or build.count('--tag')!=1 or build[build.index('--label')+1]!='org.opencontainers.image.revision='+generation or not re.fullmatch(r'[a-zA-Z0-9.-]+(?::[0-9]+)?/[a-zA-Z0-9._/-]+:'+generation,build[build.index('--tag')+1]):
        raise ValueError('failed stage native build is not bound to requested generation')
    manifest_raw=originals['manifest.json']['original'].encode()
    if digest(manifest_raw)!=originals['digest']['original'].strip():
        raise ValueError('failed original manifest digest differs')
    manifest=json.loads(manifest_raw)
    descriptors(manifest)
    config_raw=originals['image-config.json']['original'].encode()
    if digest(config_raw)!=manifest['config']['digest'] or len(config_raw)!=manifest['config']['size']:
        raise ValueError('failed original image configuration descriptor differs')
    config=json.loads(config_raw)
    if (config.get('os'),config.get('architecture'))!=('linux','amd64') or config.get('config',{}).get('Labels',{}).get('org.opencontainers.image.revision')!=generation:
        raise ValueError('failed original image platform or generation differs')
    for name in ['build-inspect.stdout','inspect.stdout']:
        inspection=json.loads(originals[name]['original'])
        if not isinstance(inspection,list) or len(inspection)!=1 or not isinstance(inspection[0],dict) or not re.fullmatch(r'sha256:[0-9a-f]{64}',inspection[0].get('Id','')):
            raise ValueError('failed native image inspection is incomplete or unknown')
    built=json.loads(originals['build-inspect.stdout']['original'])[0]
    if isinstance(built.get('Descriptor'),dict) and built['Descriptor'].get('mediaType') in INDEX_TYPES:
        path=trusted_path(stage/'tag.json')
        if not path.is_file() or path.stat().st_size>1024**2:
            raise ValueError('failed native index original is absent or exceeds the bound')
        raw=regular(path)
        native_image_identity(built,manifest_raw,config,raw,True)
        originals['tag.json']={'original':raw.decode(),'bytes':len(raw),'sha256':hashlib.sha256(raw).hexdigest()}
    return {**result,'stage_binding':'unique-generation-originals','stage':str(stage),
            'identity_originals':originals,'launch_ready':False}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('mode', choices=['inputs', 'descriptors', 'verify', 'command', 'publish', 'status', 'trusted-path', 'capacity', 'retain-blobs', 'retire-layers', 'image-inputs', 'find-reuse', 'registry-url'])
    parser.add_argument('directory', type=Path)
    parser.add_argument('--generation')
    parser.add_argument('--repository')
    parser.add_argument('--version')
    parser.add_argument('--helper', type=Path)
    parser.add_argument('--toolchain')
    parser.add_argument('--url')
    parser.add_argument('--scheme')
    parser.add_argument('--inputs-file',type=Path)
    parser.add_argument('--attempt-store',type=Path)
    parser.add_argument('--phase')
    parser.add_argument('--exit-code', type=int)
    args, extra = parser.parse_known_args()
    if extra and args.mode != 'command':
        parser.error('unexpected arguments')
    if args.mode == 'registry-url':
        print(registry_url(args.url,args.scheme,[args.repository,args.toolchain]))
    elif args.mode == 'image-inputs':
        print(json.dumps(image_inputs(args.directory,args.toolchain)))
    elif args.mode == 'find-reuse':
        reuse=find_reuse(args.directory,json.loads(regular(args.inputs_file)))
        print(json.dumps(reuse))
    elif args.mode == 'capacity':
        publication_capacity(args.directory, shutil.disk_usage(args.directory).free)
    elif args.mode == 'trusted-path':
        trusted_path(args.directory)
    elif args.mode == 'inputs':
        print(*source_inputs(args.directory), sep='\n')
    elif args.mode == 'descriptors':
        for name, row in descriptors(json.loads(regular(args.directory/'manifest.json'))):
            print(name, row['digest'], row['size'], sep='\t')
    elif args.mode == 'verify':
        verified(args.directory)
        retain_verified_blobs(args.directory)
    elif args.mode == 'retain-blobs':
        retain_verified_blobs(args.directory)
    elif args.mode == 'retire-layers':
        witness(args.directory,args.generation,args.repository,args.version,args.helper)
        for path in args.directory.glob('layer-*.blob'):
            path.unlink()
    elif args.mode == 'command':
        argv = extra[1:] if extra[:1] == ['--'] else extra
        with (args.directory/'native-commands.jsonl').open('a') as handle:
            handle.write(json.dumps({'phase': args.phase, 'exit': args.exit_code, 'argv': ['/usr/bin/docker', '--host', 'unix:///var/run/docker.sock', *argv]})+'\n')
    elif args.mode == 'status':
        if not args.directory.exists() and args.attempt_store:
            trusted_path(args.attempt_store)
            candidates=list(args.attempt_store.glob('agent-publication.'+args.directory.name+'.????????'))
            if not candidates:
                raise ValueError('no immutable witness or completed publication attempt')
            if len(candidates)!=1:
                raise ValueError('publication attempts are ambiguous')
            attempt=candidates[0]
            trusted_path(attempt)
            for path in [attempt/'receipt.json',attempt/'stdout',attempt/'stderr']:
                trusted_path(path)
            print(json.dumps(failed_image_originals(attempt,args.directory.name,args.directory.parent)))
            return
        trusted_path(args.directory)
        root = args.directory/'agent'
        for path in [root, *root.iterdir()]:
            stat = path.lstat()
            if path.is_symlink() or stat.st_uid != 0 or stat.st_mode & 0o022:
                raise ValueError('published readiness is not root-owned and non-writable')
        record = readiness(root)
        if record['generation'] != args.directory.name:
            raise ValueError('named source generation differs')
        print(json.dumps(record))
    else:
        record = witness(args.directory, args.generation, args.repository, args.version, args.helper)
        output = args.directory/'image-witness.json'
        with output.open('x') as handle:
            handle.write(json.dumps(record, indent=2)+'\n')
        print(json.dumps(record))


if __name__ == '__main__': main()
