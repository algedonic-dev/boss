#!/usr/bin/env python3
"""Remote archive integrity only; this does not restore a database (3a68ab36).

GCS JSON API objects.list is lexical, not chronological. Read every page,
then fetch the newest database object's exact generation. API contract:
https://docs.cloud.google.com/storage/docs/json_api/v1/objects/list
"""
import datetime
from collections import deque
import errno
import gzip
import hashlib
import json
import re
import signal
import subprocess
import sys
import tempfile
import zlib
from urllib.parse import quote, urlencode
from urllib.request import Request, urlopen
from urllib.error import HTTPError


CHUNK_BYTES = 64 * 1024


class CompletionTrailer:
    """Keep two meaningful lines, never a whole SQL row or archive.

    Leading/trailing whitespace can cross chunks. Retain only bounded interior
    bytes; an oversized row is meaningful but cannot be the completion marker.
    An oversized restrict header is unprovable rather than silently ignored.
    """
    def __init__(self):
        self.line, self.pending = bytearray(), bytearray()
        self.overflow = self.pending_overflow = self.header_overflow = False
        self.tail, self.index, self.restrict_key = deque(maxlen=2), 0, None

    def _part(self, part):
        if not self.line:
            part = part.lstrip()
        content = part.rstrip()
        if content:
            self.overflow |= self.pending_overflow
            for data in (self.pending, content):
                room = 4096 - len(self.line)
                self.line.extend(data[:room])
                self.overflow |= len(data) > room
            self.pending.clear()
            self.pending_overflow = False
        trailing = part[len(content):]
        room = 4096 - len(self.pending)
        self.pending.extend(trailing[:room])
        self.pending_overflow |= len(trailing) > room

    def _line(self):
        line = bytes(self.line)
        if self.index < 20:
            if self.overflow and line.startswith(b'\\restrict '):
                self.header_overflow = True
            elif re.fullmatch(rb'\\restrict [a-zA-Z0-9]+', line):
                self.restrict_key = line.split(b' ')[1]
        if line and line != b'--':
            self.tail.append(b'<oversized SQL line>' if self.overflow else line)
        self.index += 1
        self.line.clear()
        self.pending.clear()
        self.overflow = self.pending_overflow = False

    def feed(self, block):
        parts = block.split(b'\n')
        for part in parts[:-1]:
            self._part(part)
            self._line()
        self._part(parts[-1])

    def valid(self):
        self._line()
        trailer = b'-- PostgreSQL database dump complete'
        if self.header_overflow:
            return False
        if self.restrict_key is not None:
            return list(self.tail) == [trailer, b'\\unrestrict ' + self.restrict_key]
        return bool(self.tail) and self.tail[-1] == trailer


def verify(bucket, token, work, receipt=None):
    if receipt is None:
        receipt = {}
    receipt.update(claim='remote-archive-integrity', bucket=bucket, result='failed', stage='complete-list')
    base = 'https://storage.googleapis.com/storage/v1/b/' + quote(bucket, safe='') + '/o'
    stage = 'complete-list'
    candidates, seen, pages, listed = [], set(), 0, 0
    page_token = None
    while True:
        params = {'prefix': 'boss-', 'maxResults': '1000'}
        if page_token is not None:
            params['pageToken'] = page_token
        request = Request(base + '?' + urlencode(params), headers={'Authorization': 'Bearer ' + token})
        with urlopen(request, timeout=60) as response:
            page = json.load(response)
        if not isinstance(page, dict) or page.get('kind') != 'storage#objects':
            raise ValueError(stage + ': response is not an objects list')
        items = page.get('items', [])
        if not isinstance(items, list):
            raise ValueError(stage + ': items is not an array')
        pages += 1
        listed += len(items)
        receipt.update(listed_pages=pages, listed_objects=listed)
        for item in items:
            if not isinstance(item, dict) or not isinstance(item.get('name'), str):
                raise ValueError(stage + ': malformed object')
            if not re.fullmatch(r'boss-[0-9]{8}-[0-9]{6}\.sql\.gz', item['name']):
                continue
            for key in ('generation', 'size'):
                if not isinstance(item.get(key), str) or not re.fullmatch(r'[0-9]+', item[key]):
                    raise ValueError(stage + ': database object has invalid ' + key)
            if int(item['size']) <= 0 or int(item['generation']) <= 0 or item.get('bucket') != bucket:
                raise ValueError(stage + ': database object has invalid size, generation or bucket')
            timestamp = item.get('timeCreated')
            if not isinstance(timestamp, str):
                raise ValueError(stage + ': database object has invalid creation timestamp')
            try:
                created = datetime.datetime.fromisoformat(timestamp.replace('Z', '+00:00'))
            except ValueError:
                # A parser's exception can quote the provider's raw value.
                raise ValueError(stage + ': database object has invalid creation timestamp') from None
            if created.tzinfo is None:
                raise ValueError(stage + ': database object has invalid creation timestamp')
            candidates.append((created, item['name'], int(item['generation']), item))
        page_token = page.get('nextPageToken')
        if page_token is None:
            break
        if not isinstance(page_token, str) or not page_token or page_token in seen:
            raise ValueError(stage + ': invalid or repeated page token')
        seen.add(page_token)
    if not candidates:
        raise ValueError(stage + ': no database dump in the complete listing')
    selected = max(candidates, key=lambda candidate: candidate[:3])[3]
    receipt.update(object=selected['name'],
                   generation=selected['generation'], object_created_at=selected['timeCreated'],
                   object_bytes=int(selected['size']), listed_pages=pages, listed_objects=listed,
                   matching_dumps=len(candidates), selected_metadata={key: selected[key] for key in
                       ('name', 'bucket', 'generation', 'metageneration', 'size', 'timeCreated',
                        'updated', 'md5Hash', 'crc32c', 'contentType', 'storageClass') if key in selected})
    # Pin both the requested version and its precondition: a vanished or replaced
    # generation is a failure, never a second selection against a moving bucket.
    params = dict(alt='media', generation=selected['generation'], ifGenerationMatch=selected['generation'])
    request = Request(base + '/' + quote(selected['name'], safe='') + '?' + urlencode(params),
                      headers={'Authorization': 'Bearer ' + token})
    downloaded, digest = 0, hashlib.sha256()
    checker = None
    receipt['stage'] = 'pinned-download'
    # A timeout or hard kill must not erase the only selected identity.
    # The ops runner stores this flushed checkpoint even if no final receipt
    # can be emitted. Its claim remains failed until every check completes.
    print('checkpoint: ' + json.dumps(receipt, sort_keys=True), flush=True)
    try:
        with urlopen(request, timeout=60) as response:
            # Both validators consume the same pinned bytes. gzip -t sees the
            # complete compressed stream; Python validates CRC/EOF while giving
            # the trailer reader bounded decoded blocks. No archive is spooled.
            checker = subprocess.Popen(['gzip', '-t'], stdin=subprocess.PIPE,
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, bufsize=0)
            class PinnedBytes:
                def read(self, size=-1):
                    nonlocal downloaded
                    block = response.read(CHUNK_BYTES if size < 0 else min(size, CHUNK_BYTES))
                    downloaded += len(block)
                    receipt['downloaded_bytes'] = downloaded
                    if downloaded > receipt['object_bytes']:
                        raise ValueError('download: more bytes than selected metadata')
                    digest.update(block)
                    try:
                        remaining = memoryview(block)
                        while remaining:
                            written = checker.stdin.write(remaining)
                            if not written:
                                raise OSError(errno.EIO, 'native gzip pipe made no progress')
                            remaining = remaining[written:]
                    except BrokenPipeError:
                        receipt['stage'] = 'gzip-test'
                        raise ValueError('gzip-test: native validator refused the compressed stream') from None
                    return block
            trailer = CompletionTrailer()
            try:
                with gzip.GzipFile(fileobj=PinnedBytes(), mode='rb') as stream:
                    while True:
                        block = stream.read(CHUNK_BYTES)
                        if not block:
                            break
                        trailer.feed(block)
            except (gzip.BadGzipFile, EOFError, zlib.error) as error:
                receipt.update(stage='gzip-test', gzip_error_type=type(error).__name__)
                raise ValueError('gzip-test: downloaded generation is not a whole gzip stream') from None
        if downloaded != receipt['object_bytes']:
            raise ValueError('download: length differs from selected metadata')
        receipt['stage'] = 'gzip-test'
        checker.stdin.close()
        receipt['gzip_test_exit_code'] = checker.wait(timeout=60)
        if receipt['gzip_test_exit_code']:
            raise ValueError('gzip-test: downloaded generation is not a whole gzip stream')
        # Only the final meaningful comment can prove pg_dump completed. A
        # substring in SQL, an earlier comment, or a marker followed by SQL cannot.
        receipt['gzip_test'] = True
        receipt['stage'] = 'completion-trailer'
        if not trailer.valid():
            raise ValueError('completion-trailer: pg_dump trailer and optional matching unrestrict footer are absent at the end')
        receipt.update(result='ok', stage='verified', downloaded_bytes=downloaded, sha256=digest.hexdigest(), gzip_test=True,
                       completion_trailer=True, verification_mode='bounded-stream',
                       checked_at=datetime.datetime.now(datetime.timezone.utc).isoformat())
        return receipt
    finally:
        if checker is not None:
            if not checker.stdin.closed:
                checker.stdin.close()
            try:
                receipt['gzip_test_exit_code'] = checker.wait(timeout=5)
            except subprocess.TimeoutExpired:
                checker.kill()
                receipt['gzip_test_exit_code'] = checker.wait(timeout=5)


def main():
    receipt = dict(claim='remote-archive-integrity', result='failed', stage='configuration')
    def interrupted(signum, frame):
        raise TimeoutError('bounded termination')
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        with open('/gcs/bucket', encoding='utf-8') as source:
            bucket = source.read().strip()
        if not re.fullmatch(r'[a-z0-9][a-z0-9._-]{1,220}[a-z0-9]', bucket):
            raise ValueError('configuration: Secret bucket is not a bucket name')
        # Credentials stay inside this container's tmpfs, never argv or output.
        receipt['stage'] = 'authentication'
        auth = subprocess.run(['gcloud', 'auth', 'activate-service-account', '--key-file=/gcs/sa.json', '--quiet'],
                              capture_output=True, timeout=60, check=False)
        if auth.returncode:
            raise ValueError('authentication: existing Secret could not authenticate')
        access = subprocess.run(['gcloud', 'auth', 'print-access-token'], capture_output=True, timeout=60, check=False)
        if access.returncode or not access.stdout.strip():
            raise ValueError('authentication: access token unavailable')
        with tempfile.TemporaryDirectory(dir='/work') as work:
            verify(bucket, access.stdout.decode().strip(), work, receipt)
        print('receipt: ' + json.dumps(receipt, sort_keys=True), flush=True)
        return 0
    except Exception as error:
        # HTTP bodies and gcloud stderr can carry credentials. The type and
        # bounded stage explain refusal; never print provider response bodies.
        reason = str(error) if isinstance(error, ValueError) else type(error).__name__
        if isinstance(error, HTTPError):
            receipt['http_status'] = error.code
        if isinstance(error, OSError) and isinstance(error.errno, int):
            receipt.update(errno=error.errno, errno_name=errno.errorcode.get(error.errno, 'UNKNOWN'))
        receipt.update(result='failed', error=reason, checked_at=datetime.datetime.now(datetime.timezone.utc).isoformat())
        print('receipt: ' + json.dumps(receipt, sort_keys=True), flush=True)
        print('check-gcs-backup: FAILED — ' + reason, file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
