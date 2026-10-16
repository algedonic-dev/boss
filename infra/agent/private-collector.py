#!/usr/bin/python3 -I
"""Fixed private TLS upload. Public output is a bounded receipt, never diagnostics.

The handler and its independent native authorization are separately installed.
An unavailable handler refuses before the retained cache is opened. A successful
transport receipt does not prove account validation or generation publication.
"""
import hashlib
import http.client
import json
import os
from pathlib import Path
import ssl
import stat
import time
import uuid

LIMIT = 256 * 1024
IDENTITY_KEYS = {'run', 'nonce', 'pod_uid', 'workspace_uid'}
CONTRACT = Path('/usr/share/boss-agent/private-collector-contract.json')


def collect(identity, transport, private_read):
    result = {'schema': 1, **identity, 'status': 'incomplete', 'code': 'collection-unconfirmed'}
    try:
        authorized = transport('authorize', b'')
        if not isinstance(authorized, dict) or type(authorized.get('schema')) is not int or authorized != {'schema': 1, **identity, 'status': 'authorized'}:
            return result
        private = private_read()
        if not isinstance(private, bytes) or not 0 < len(private) <= LIMIT:
            return result
        digest = hashlib.sha256(private).hexdigest()
        retained = transport('collect', private)
        expected = {'schema': 1, **identity, 'status': 'retained', 'sha256': digest, 'bytes': len(private)}
        if not isinstance(retained, dict) or type(retained.get('schema')) is not int or retained != expected or type(retained.get('bytes')) is not int:
            return result
        return {'schema': 1, **identity, 'status': 'complete', 'code': 'private-retention-acknowledged', 'sha256': digest, 'bytes': len(private)}
    except Exception:
        return result


def directory(path):
    """Walk absolute directories through descriptors, rejecting every symlink."""
    parts = Path(path).parts
    if not parts or parts[0] != '/':
        raise ValueError('private-path')
    fd = os.open('/', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for part in parts[1:]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = child
        return fd
    except Exception:
        os.close(fd)
        raise


def read_file(parent, name, owner, mode, limit):
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
    try:
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_uid != owner or before.st_nlink != 1 or stat.S_IMODE(before.st_mode) != mode or not 0 < before.st_size <= limit:
            raise ValueError('private-file')
        chunks = []
        remaining = limit + 1
        while remaining:
            chunk = os.read(fd, min(65536, remaining))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        raw = b''.join(chunks)
        after = os.fstat(fd)
        if len(raw) != before.st_size or len(raw) > limit or (before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns, before.st_nlink) != (after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns, after.st_nlink):
            raise ValueError('private-file-changed')
        return raw
    finally:
        os.close(fd)


def private_cache(path):
    parent = directory(str(Path(path).parent))
    try:
        info = os.fstat(parent)
        if info.st_uid != 65534 or stat.S_IMODE(info.st_mode) != 0o700:
            raise ValueError('private-directory')
        return read_file(parent, Path(path).name, 65534, 0o600, LIMIT)
    finally:
        os.close(parent)


def projection(path):
    """Pin a Kubernetes atomic-writer generation; no arbitrary link traversal."""
    parent = directory(path)
    try:
        info = os.fstat(parent)
        if info.st_uid != 0 or info.st_mode & 0o022:
            raise ValueError('projection-directory')
        generation = os.readlink('..data', dir_fd=parent)
        if not generation.startswith('..') or generation in ('.', '..') or '/' in generation:
            raise ValueError('projection-generation')
        child = os.open(generation, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
        try:
            return read_file(child, 'token', 0, 0o440, 16384), read_file(child, 'ca.crt', 0, 0o440, 65536)
        finally:
            os.close(child)
    finally:
        os.close(parent)


class FixedTransport:
    def __init__(self, contract, identity, token, ca):
        self.contract = contract
        self.identity = identity
        self.token = token.decode('ascii').strip()
        if not self.token or any(c.isspace() for c in self.token):
            raise ValueError('projection-token')
        # create_default_context honors SSLKEYLOGFILE; private transport must not.
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        self.context.check_hostname = True
        self.context.verify_mode = ssl.CERT_REQUIRED
        self.context.load_verify_locations(cadata=ca.decode('ascii'))
        self.deadline = time.monotonic() + contract['deadline_seconds']

    def __call__(self, phase, body):
        timeout = min(15, self.deadline - time.monotonic())
        if timeout <= 0 or phase not in ('authorize', 'collect') or len(body) > LIMIT:
            raise ValueError('collection-bound')
        # Direct stdlib connection ignores proxy environment. No redirects/retries.
        connection = http.client.HTTPSConnection(self.contract['host'], self.contract['port'], timeout=timeout, context=self.context)
        try:
            headers = {'Authorization': 'Bearer ' + self.token, 'Content-Type': 'application/octet-stream', 'X-BOSS-Collection-Phase': phase}
            headers.update({'X-BOSS-' + key.replace('_', '-'): value for key, value in self.identity.items()})
            connection.request('POST', self.contract['path'], body=body, headers=headers)
            response = connection.getresponse()
            if response.status != 200:
                raise ValueError('collection-refused')
            raw = response.read(4097)
            if len(raw) > 4096:
                raise ValueError('collection-reply-bound')
            return json.loads(raw)
        finally:
            connection.close()


def main():
    identity = {}
    try:
        # Image-owned public contract, never a URL/path supplied by a worker.
        contract = json.loads(CONTRACT.read_bytes())
        identity = {key: os.environ['BOSS_COLLECTION_' + key.upper()] for key in IDENTITY_KEYS}
        for value in identity.values():
            if str(uuid.UUID(value)) != value or uuid.UUID(value).int == 0:
                raise ValueError('collection-identity')
        token, ca = projection(contract['projection'])
        transport = FixedTransport(contract, identity, token, ca)
        result = collect(identity, transport, lambda: private_cache(contract['cache']))
    except Exception:
        result = {'schema': 1, 'status': 'incomplete', 'code': 'collection-unconfirmed'}
    print(json.dumps(result, sort_keys=True, separators=(',', ':')))
    return 0 if result['status'] == 'complete' else 75


if __name__ == '__main__':
    raise SystemExit(main())
