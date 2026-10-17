#!/usr/bin/env python3
"""The fixed planner's bounded original-byte carrier (urgent 1200f7a7).

The ops receipt carries references, never an unbounded native roster. The
existing `boss attach` door verifies each original part before acknowledging
it. This module grants no resource operation and accepts no remote target.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import zlib
import stat

PART_BYTES = 1048576  # below the existing file door's 2 MiB body limit
MAX_ENVELOPE = 8 * PART_BYTES
MAX_ORIGINAL = 32 * PART_BYTES
MAX_PACKET = PART_BYTES
MAX_STREAM = 131072  # decoder safety bound, not the runner's capture policy


def need(condition, why):
    if not condition:
        raise ValueError('native evidence: '+why)


def identity(value):
    need(type(value) is str and re.fullmatch(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', value),
         'request or file identity malformed')
    return value


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def document(raw):
    def unique(pairs):
        value={}
        for key,item in pairs:
            need(key not in value,'duplicate JSON key')
            value[key]=item
        return value
    try:
        return json.loads(raw,object_pairs_hook=unique)
    except (ValueError,UnicodeError) as error:
        raise ValueError('native evidence: not one whole JSON document') from error


def originals(raw, request, include_failure=False):
    identity(request)
    need(type(raw) is bytes and 0<len(raw)<=MAX_ENVELOPE,'envelope exceeds bounded original carrier')
    value=document(raw)
    need(type(value) is dict and value.get('complete') is True
         and value.get('evidence_encoding')=='gzip+base64' and value.get('request_id')==request,
         'incomplete, unbound or unsupported envelope')
    size=value.get('original_bytes')
    need(type(size) is int and 0<size<=MAX_ORIGINAL,'decoded originals exceed bound')
    try:
        compressed=base64.b64decode(value['data'],validate=True)
        decoder=zlib.decompressobj(16+zlib.MAX_WBITS)
        decoded=decoder.decompress(compressed,size+1)
        need(len(decoded)==size and decoder.eof and not decoder.unused_data and not decoder.unconsumed_tail,
             'compression is partial, multiple, over bound or has trailing bytes')
    except (KeyError,TypeError,ValueError,zlib.error) as error:
        raise ValueError('native evidence: malformed or incomplete compression') from error
    need(sha(decoded)==value.get('original_sha256'),'original hash mismatch')
    records=document(decoded)
    need(type(records) is list and len(records)<=2048,'original invocation roster exceeds bound')
    for record in records:
        need(type(record) is dict and type(record.get('argv')) is list and record['argv']
             and all(type(arg) is str for arg in record['argv']), 'original command record malformed')
        need('exit' in record and (record['exit'] is None or type(record['exit']) is int),
             'original exit missing or malformed')
        for stream in ('stdout','stderr'):
            try:
                data=base64.b64decode(record[stream+'_b64'],validate=True)
                need(type(record[stream+'_bytes']) is int and len(data)==record[stream+'_bytes']
                     and sha(data)==record[stream+'_sha256'],'original stream count/hash mismatch')
            except (KeyError,TypeError,ValueError) as error:
                raise ValueError('native evidence: original stream is incomplete or changed') from error
    need(value.get('failure') is None or type(value['failure']) is str,'refusal diagnostic malformed')
    if include_failure:
        return {'request_id':request,'failure':value.get('failure'),'records':records}
    return records


def deliver(raw, request, put, failure=None):
    """No reference is complete until every bounded original part is verified."""
    originals(raw,request)
    need(failure is None or type(failure) is str,'refusal diagnostic malformed')
    parts=[]
    for offset in range(0,len(raw),PART_BYTES):
        part=raw[offset:offset+PART_BYTES]
        try:
            file_id=identity(put(part))
        except (OSError,ValueError,subprocess.SubprocessError) as error:
            # No word on effect: only the planner knows whether a patch was sent.
            raise ValueError('native evidence: attachment not completely verified; originals retained') from error
        parts.append({'file_id':file_id,'bytes':len(part),'sha256':sha(part)})
    need(len({p['file_id'] for p in parts})==len(parts),'attachment identities repeated')
    diagnostic=failure if failure is None or len(failure)<=4000 else 'refused; complete original diagnostic is in the attachment'
    return {'evidence_encoding':'attached-native-v1','request_id':request,'complete':True,
            'bytes':len(raw),'sha256':sha(raw),'parts':parts,'failure':diagnostic}


def restore(reference, request, get, include_failure=False):
    """A caller downloads file ids through the existing file reader, then judges."""
    identity(request)
    if type(reference) is dict and reference.get('evidence_encoding')=='gzip+base64':
        return originals(json.dumps(reference).encode(),request,include_failure)
    need(type(reference) is dict and reference.get('evidence_encoding')=='attached-native-v1'
         and reference.get('request_id')==request and reference.get('complete') is True,
         'reference is partial, replayed or unsupported')
    parts=reference.get('parts')
    need(type(parts) is list and 1<=len(parts)<=MAX_ENVELOPE//PART_BYTES,'part roster exceeds bound')
    raw=b'';seen=set()
    for index,part in enumerate(parts):
        need(type(part) is dict,'part metadata missing')
        file_id=identity(part.get('file_id'))
        need(file_id not in seen,'part identity repeated')
        seen.add(file_id)
        size=part.get('bytes')
        need(type(size) is int and 0<size<=PART_BYTES
             and (index==len(parts)-1 or size==PART_BYTES),'part count/order malformed')
        try:
            chunk=get(file_id)
        except (OSError,KeyError) as error:
            raise ValueError('native evidence: attachment unavailable') from error
        need(type(chunk) is bytes and len(chunk)==size and sha(chunk)==part.get('sha256'),
             'part is missing, partial or changed')
        raw+=chunk
    need(len(raw)==reference.get('bytes') and sha(raw)==reference.get('sha256'),'whole envelope count/hash mismatch')
    return originals(raw,request,include_failure)


def from_packet(packet, request):
    """Verify the actual runner carrier before interpreting its one result.

    A failed plan is useful diagnostic evidence. It is never promoted to a
    successful proposal merely because its originals have arrived intact.
    """
    identity(request)
    need(type(packet) is dict and packet.get('id')==request and packet.get('kind')=='ops-request'
         and packet.get('status')=='closed' and packet.get('simulated') is False,
         'ops-request is incomplete, simulated or names another request')
    metadata=packet.get('metadata',{})
    need(type(metadata) is dict and metadata.get('host')=='forge' and metadata.get('verb')=='plan-dev-work-volume-expansion'
         and type(metadata.get('args')) is list and len(metadata['args'])==1
         and type(metadata['args'][0]) is str
         and re.fullmatch(r'(4[1-9]|5[0-9]|6[0-4])Gi',metadata['args'][0]),
         'ops-request is not the fixed forge volume planner')
    steps=packet.get('steps')
    need(type(steps) is list,'ops execution record missing')
    matches=[step for step in steps if type(step) is dict and step.get('spec_slug')=='execute']
    need(len(matches)==1 and matches[0].get('status')=='completed','ops execute step is not uniquely completed')
    meta=matches[0].get('metadata',{})
    need(type(meta) is dict and meta.get('disposition')=='answered','ops execution did not answer')
    need(not meta.get('streams_unread'),'ops runner could not retain separate streams')
    exit_code=meta.get('exit_code')
    need(type(exit_code) in (int,str) and re.fullmatch(r'[0-9]{1,3}',str(exit_code))
         and int(exit_code)<=255,'ops exit is missing or malformed')
    code=int(exit_code)
    streams=meta.get('streams')
    if type(streams) is str:streams=document(streams)
    need(type(streams) is dict and set(streams)=={'stdout','stderr'},'ops separate streams unavailable')
    decoded={}
    for name in ('stdout','stderr'):
        stream=streams[name]
        need(type(stream) is dict and stream.get('complete') is True,
             'ops '+name+' is incomplete')
        count=stream.get('bytes')
        need(type(count) is int and 0<=count<=MAX_STREAM
             and type(stream.get('retained_bytes')) is int and stream['retained_bytes']==count,
             'ops '+name+' byte count is incomplete or over bound')
        try:data=base64.b64decode(stream['base64'],validate=True)
        except (KeyError,ValueError,TypeError) as error:raise ValueError('native evidence: malformed ops stream') from error
        need(len(data)==count and sha(data)==stream.get('sha256'),'ops '+name+' count/hash mismatch')
        decoded[name]=data
    references=[]
    for line in decoded['stderr'].splitlines():
        if line.lstrip().startswith(b'{'):
            value=document(line)
            need(type(value) is dict and value.get('evidence_encoding') in ('attached-native-v1','gzip+base64'),
                 'unexpected JSON document in native evidence carrier')
            references.append(value)
    need(len(references)==1,'native evidence document missing or repeated')
    reference=references[0]
    need(reference.get('request_id')==request,'evidence document names another request')
    proposal=None
    if code==0:
        proposal=document(decoded['stdout'])
        need(type(proposal) is dict and proposal.get('purpose')=='dev-work-volume',
             'successful native plan is not one fixed-purpose proposal')
        hashes=re.findall(rb'^plan-sha256: ([0-9a-f]{64})$',decoded['stderr'],re.M)
        need(len(hashes)==1 and hashes[0].decode()==sha(decoded['stdout']),
             'successful native proposal hash is missing or differs')
        need(not reference.get('failure'),'successful carrier contains a refusal')
    return reference,{'state':'verified-ops-carrier','exit':code,'plan_success':code==0,
                      'plan':proposal,'stdout_b64':base64.b64encode(decoded['stdout']).decode(),
                      'stderr_b64':base64.b64encode(decoded['stderr']).decode()}


def read_part(directory, file_id):
    """The offline consumer never loads an unbounded or redirected file."""
    identity(file_id)
    try:
        descriptor=os.open(Path(directory)/file_id,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
        with os.fdopen(descriptor,'rb') as source:
            need(stat.S_ISREG(os.fstat(source.fileno()).st_mode),'part is not a regular file')
            raw=source.read(PART_BYTES+1)
    except OSError as error:
        raise ValueError('native evidence: part unavailable or redirected') from error
    need(len(raw)<=PART_BYTES,'part exceeds byte bound')
    return raw


def publish(part, request, directory):
    """Reuse the installed actor-signing, readback-verifying attachment door."""
    identity(request)
    need(type(part) is bytes and 0<len(part)<=PART_BYTES,'attachment exceeds bound')
    path=directory/(sha(part)+'.native-evidence')
    if path.exists():
        # Equal chunks are legitimate. A new verified attachment is still
        # bound to its own ordered slot; never overwrite an earlier receipt.
        path=directory/(sha(part)+'-'+str(len(list(directory.iterdir())))+'.native-evidence')
    with path.open('xb') as target:
        os.chmod(path,0o600)
        target.write(part);target.flush();os.fsync(target.fileno())
    descriptor=os.open(directory,os.O_RDONLY|os.O_DIRECTORY)
    try:os.fsync(descriptor)
    finally:os.close(descriptor)
    code=None
    try:
        result=subprocess.run(['/usr/local/bin/boss','attach',request,str(path)],
                              capture_output=True,timeout=60,check=False)
        stdout,stderr,code=result.stdout,result.stderr,result.returncode
    except subprocess.TimeoutExpired as error:
        stdout,stderr=error.stdout or b'',error.stderr or b''
    # Preserve failures in the protected invocation directory, not an excerpt
    # in an immutable event. The CLI itself compares the store readback bytes.
    (directory/(path.name+'.stdout')).write_bytes(stdout)
    (directory/(path.name+'.stderr')).write_bytes(stderr)
    (directory/(path.name+'.exit.json')).write_text(json.dumps({'exit':code})+'\n')
    need(code==0 and len(stdout)<=16384 and len(stderr)<=16384,
         'existing attachment door failed; protected originals retained')
    matches=re.findall(rb'as file ([0-9a-f-]{36}),',stdout)
    need(len(matches)==1 and b'byte-identical' in stdout
         and sha(part).encode() in stdout,'attachment receipt is not complete')
    return identity(matches[0].decode())


def record(raw, request, failure=None, root=Path('/var/lib/boss/dev-work-evidence')):
    """Keep the originals even if the existing file service cannot answer."""
    identity(request)
    need(type(raw) is bytes and raw,'envelope missing')
    root.mkdir(mode=0o700,parents=True,exist_ok=True)
    need(not root.is_symlink() and root.stat().st_uid==os.getuid() and root.stat().st_mode&0o077==0,
         'original evidence directory is not private and owned')
    directory=Path(tempfile.mkdtemp(prefix=request+'-',dir=root))
    os.chmod(directory,0o700)
    with (directory/'envelope.json').open('xb') as original:
        original.write(raw);original.flush();os.fsync(original.fileno())
    descriptor=os.open(directory,os.O_RDONLY|os.O_DIRECTORY)
    try:os.fsync(descriptor)
    finally:os.close(descriptor)
    # Review b9075eb9 (31d76e37): a roster can outgrow the planner's room
    # projection after a patch. Refuse over-bound originals only once they
    # are on disk here; they are the only copy.
    need(len(raw)<=MAX_ENVELOPE,'envelope exceeds bounded original carrier; originals retained in '+str(directory))
    reference=deliver(raw,request,lambda part:publish(part,request,directory),failure)
    (directory/'reference.json').write_text(json.dumps(reference)+'\n')
    return reference
