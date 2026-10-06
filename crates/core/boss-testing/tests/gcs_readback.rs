//! Read the remote generation, not the bytes the uploader already had (3a68ab36).
use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::process::Command;

#[test]
fn remote_readback_pages_pins_and_checks_the_actual_trailer() {
    let dir = scratch_dir("gcs-readback");
    let test = dir.join("test.py");
    write_file(&test, TEST);
    let output = Command::new("python3")
        .arg(&test)
        .arg(repo_root().join("infra/forge/gcs-readback.py"))
        .env("TMPDIR", &dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(dir);
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

const TEST: &str = r#"
import base64, contextlib, errno, gzip, hashlib, importlib.util, io, json, os, random, signal, sys, tempfile, threading, time, unittest
from types import SimpleNamespace
from urllib.error import HTTPError
from unittest.mock import patch
from urllib.parse import urlparse, parse_qs
spec = importlib.util.spec_from_file_location('readback', sys.argv.pop())
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

def obj(name, time, generation='44', size=1):
    return dict(name=name, timeCreated=time, generation=generation, size=str(size), bucket='test-bucket')

class Readback(unittest.TestCase):
    def run_check(self, body, mode='ok'):
        blob = gzip.compress(body) if mode != 'corrupt' else b'not a gzip'
        old = obj('boss-20261001-091000.sql.gz', '2026-10-01T09:10:00Z')
        newest = obj('boss-20261002-091000.sql.gz', '2026-10-02T09:10:00.123Z', size=len(blob))
        calls = []
        def read(request, timeout):
            calls.append(request.full_url)
            query = parse_qs(urlparse(request.full_url).query)
            if query.get('alt') == ['media']:
                self.assertEqual(query['generation'], ['44'])
                self.assertEqual(query['ifGenerationMatch'], ['44'])
                self.assertIn(newest['name'], request.full_url)
                if mode == 'fetch-fail': raise OSError('network down')
                return io.BytesIO(blob[:-3] if mode == 'short' else blob)
            if 'pageToken' not in query:
                return io.BytesIO(json.dumps(dict(kind='storage#objects', items=[old], nextPageToken='tail')).encode())
            if mode == 'list-fail': raise OSError('page two down')
            if mode == 'loop': return io.BytesIO(json.dumps(dict(kind='storage#objects', nextPageToken='tail')).encode())
            if mode == 'empty': newest_items = []
            elif mode == 'bad-meta': newest_items = [dict(newest, generation='')]
            else: newest_items = [newest, obj('boss-files-20261003-091000.tar.gz', '2026-10-03T09:10:00Z')]
            return io.BytesIO(json.dumps(dict(kind='storage#objects', items=newest_items)).encode())
        with tempfile.TemporaryDirectory() as work, patch.object(m, 'urlopen', read):
            receipt = m.verify('test-bucket', 'fake-token', work)
            self.assertEqual(receipt['generation'], '44')
            self.assertEqual(receipt['object'], newest['name'])
            self.assertEqual(receipt['listed_pages'], 2)
            self.assertTrue(receipt['gzip_test'])
            self.assertTrue(receipt['completion_trailer'])
            self.assertEqual(receipt['downloaded_bytes'], len(blob))
            self.assertEqual(len(receipt['sha256']), 64)
            self.assertEqual(receipt['claim'], 'remote-archive-integrity')
        return calls
    def test_good_remote_copy(self):
        self.run_check(b'-- PostgreSQL database dump\nCREATE TABLE audit_log ();\n-- PostgreSQL database dump complete\n--\n\n')
    def test_current_pg16_restrict_footer(self):
        self.run_check(b'-- PostgreSQL database dump\n\\restrict TestKey123\nCREATE TABLE audit_log ();\n-- PostgreSQL database dump complete\n--\n\n\\unrestrict TestKey123\n\n')
        for ending in [b'\\unrestrict WrongKey', b'\\unrestrict TestKey123\nCREATE TABLE missing ();']:
            with self.assertRaises(Exception):
                self.run_check(b'-- PostgreSQL database dump\n\\restrict TestKey123\n-- PostgreSQL database dump complete\n--\n' + ending)
    def test_marker_in_body_is_not_the_trailer(self):
        with self.assertRaises(Exception): self.run_check(b'-- PostgreSQL database dump complete\nCREATE TABLE missing ();\n')
    def test_failure_never_falls_back_to_older_object(self):
        for mode in ['corrupt', 'short', 'fetch-fail', 'list-fail', 'loop', 'bad-meta']:
            with self.subTest(mode=mode), self.assertRaises(Exception):
                self.run_check(b'-- PostgreSQL database dump complete\n', mode)
    def test_empty_listing_refused(self):
        with patch.object(m, 'urlopen', lambda request, timeout: io.BytesIO(b'{"kind":"storage#objects","items":[]}')):
            with tempfile.TemporaryDirectory() as work, self.assertRaises(Exception): m.verify('test-bucket', 'fake-token', work)
    def test_failure_receipt_names_the_selected_generation_and_stage(self):
        receipt = {}
        data = obj('boss-20261002-091000.sql.gz', '2026-10-02T09:10:00Z', size=5)
        def read(request, timeout):
            if 'alt=media' in request.full_url: return io.BytesIO(b'abcde')
            return io.BytesIO(json.dumps(dict(kind='storage#objects', items=[data])).encode())
        with patch.object(m, 'urlopen', read), tempfile.TemporaryDirectory() as work:
            with self.assertRaises(Exception): m.verify('test-bucket', 'fake-token', work, receipt)
            self.assertEqual(receipt['generation'], '44')
            self.assertEqual(receipt['stage'], 'gzip-test')
            self.assertGreater(receipt['downloaded_bytes'], 0)
            self.assertLessEqual(receipt['downloaded_bytes'], 5)
            self.assertEqual(__import__('os').listdir(work), [])
    def main_failure(self, failure):
        data = obj('boss-20261002-091000.sql.gz', '2026-10-02T09:10:00Z', size=5)
        def read(request, timeout):
            if 'alt=media' in request.full_url:
                if failure == 'http': raise HTTPError(request.full_url, 403, 'Forbidden', {}, io.BytesIO(b'credential-marker'))
                time.sleep(5)
            return io.BytesIO(json.dumps(dict(kind='storage#objects', items=[data])).encode())
        original_open = open
        original_temp = tempfile.TemporaryDirectory
        def read_file(path, *args, **kwargs):
            return io.StringIO('test-bucket') if path == '/gcs/bucket' else original_open(path, *args, **kwargs)
        output, errors = io.StringIO(), io.StringIO()
        timer = threading.Timer(0.05, lambda: os.kill(os.getpid(), signal.SIGTERM)) if failure == 'timeout' else None
        if timer: timer.start()
        try:
            with patch('builtins.open', read_file), patch.object(m, 'urlopen', read), \
                 patch.object(m.subprocess, 'run', return_value=SimpleNamespace(returncode=0, stdout=b'fake-token')), \
                 patch.object(m.tempfile, 'TemporaryDirectory', side_effect=lambda **kwargs: original_temp()), \
                 contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                self.assertEqual(m.main(), 1)
        finally:
            if timer: timer.cancel()
        self.assertNotIn('credential-marker', output.getvalue() + errors.getvalue())
        lines = output.getvalue().splitlines()
        checkpoint = json.loads(next(line[12:] for line in lines if line.startswith('checkpoint: ')))
        self.assertEqual(checkpoint['generation'], '44')
        receipt = json.loads(next(line[9:] for line in lines if line.startswith('receipt: ')))
        self.assertEqual(receipt['generation'], '44')
        self.assertEqual(receipt['stage'], 'pinned-download')
        self.assertEqual(receipt['result'], 'failed')
        return receipt
    def test_bounded_termination_preserves_selected_generation(self):
        receipt = self.main_failure('timeout')
        self.assertEqual(receipt['error'], 'TimeoutError')
    def test_permission_failure_records_http_status_without_body(self):
        receipt = self.main_failure('http')
        self.assertEqual(receipt['http_status'], 403)

    def main_result(self, read, archive_open=None):
        original_open, original_run, original_temp = open, subprocess_run, tempfile.TemporaryDirectory
        def read_file(path, *args, **kwargs):
            if path == '/gcs/bucket': return io.StringIO('test-bucket')
            if archive_open and str(path).endswith('/remote.sql.gz') and args and args[0] == 'wb':
                return archive_open(original_open(path, *args, **kwargs))
            return original_open(path, *args, **kwargs)
        def run(command, **kwargs):
            if command[0] == 'gcloud': return SimpleNamespace(returncode=0, stdout=b'fake-token')
            return original_run(command, **kwargs)
        output, errors = io.StringIO(), io.StringIO()
        with patch('builtins.open', read_file), patch.object(m, 'urlopen', read), \
             patch.object(m.subprocess, 'run', run), \
             patch.object(m.tempfile, 'TemporaryDirectory', side_effect=lambda **kwargs: original_temp()), \
             contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            result = m.main()
        self.assertNotIn('credential-marker', output.getvalue() + errors.getvalue())
        receipts = [json.loads(line[9:]) for line in output.getvalue().splitlines() if line.startswith('receipt: ')]
        self.assertEqual(len(receipts), 1)
        return result, receipts[0]

    def test_archive_larger_than_the_write_port_streams_without_spooling(self):
        body = b'-- PostgreSQL database dump\nSELECT \'' + base64.b64encode(random.Random(1).randbytes(256 * 1024)) + b"';\n-- PostgreSQL database dump complete\n"
        blob = gzip.compress(body)
        capacity, writes = 64 * 1024, []
        self.assertGreater(len(blob), capacity)
        newest = obj('boss-20261002-091000.sql.gz', '2026-10-02T09:10:00Z', size=len(blob))
        class LimitedFile:
            def __init__(self, file): self.file, self.used = file, 0
            def __enter__(self): return self
            def __exit__(self, *args): self.file.close()
            def write(self, block):
                writes.append(len(block))
                if self.used + len(block) > capacity: raise OSError(errno.ENOSPC, 'credential-marker')
                self.used += len(block)
                return self.file.write(block)
        reads = []
        class BoundedMedia(io.BytesIO):
            def read(self, size=-1):
                reads.append(size)
                return super().read(size)
        def read(request, timeout):
            if 'alt=media' in request.full_url: return BoundedMedia(blob)
            return io.BytesIO(json.dumps(dict(kind='storage#objects', items=[newest])).encode())
        result, receipt = self.main_result(read, LimitedFile)
        self.assertEqual(result, 0, receipt)
        self.assertEqual(writes, [])
        self.assertEqual(receipt['downloaded_bytes'], len(blob))
        self.assertEqual(receipt['sha256'], hashlib.sha256(blob).hexdigest())
        self.assertEqual(receipt['verification_mode'], 'bounded-stream')
        self.assertTrue(receipt['gzip_test'] and receipt['completion_trailer'])
        self.assertTrue(reads)
        self.assertTrue(all(0 <= size <= capacity for size in reads))

    def test_io_failure_keeps_safe_errno_and_selected_identity(self):
        newest = obj('boss-20261002-091000.sql.gz', '2026-10-02T09:10:00Z', size=10)
        for number in [errno.ENOSPC, errno.EIO]:
            def read(request, timeout):
                if 'alt=media' in request.full_url: raise OSError(number, 'credential-marker')
                return io.BytesIO(json.dumps(dict(kind='storage#objects', items=[newest])).encode())
            result, receipt = self.main_result(read)
            self.assertEqual(result, 1)
            self.assertEqual(receipt['generation'], '44')
            self.assertEqual(receipt['stage'], 'pinned-download')
            self.assertEqual(receipt['errno'], number)
            self.assertEqual(receipt['errno_name'], errno.errorcode[number])

    def test_malformed_creation_time_refuses_without_provider_values(self):
        for value in ['credential-marker-provider-value', None, 42, {}, '2026-10-02T00:00:00', 'absent']:
            newest = obj('boss-20261002-091000.sql.gz', value, size=10)
            if value == 'absent': newest.pop('timeCreated')
            def read(request, timeout):
                self.assertNotIn('alt=media', request.full_url)
                return io.BytesIO(json.dumps(dict(kind='storage#objects', items=[newest])).encode())
            result, receipt = self.main_result(read)
            self.assertEqual(result, 1)
            self.assertEqual(receipt['stage'], 'complete-list')
            self.assertEqual(receipt['error'], 'complete-list: database object has invalid creation timestamp')
            self.assertEqual(receipt['listed_objects'], 1)

    def blob_result(self, blob, size=None):
        newest = obj('boss-20261002-091000.sql.gz', '2026-10-02T09:10:00Z', size=len(blob) if size is None else size)
        def read(request, timeout):
            if 'alt=media' in request.full_url: return io.BytesIO(blob)
            return io.BytesIO(json.dumps(dict(kind='storage#objects', items=[newest])).encode())
        return self.main_result(read)

    def test_native_validator_receives_every_byte_after_short_pipe_writes(self):
        blob = gzip.compress(random.Random(2).randbytes(128 * 1024) + b'\n-- PostgreSQL database dump complete\n')
        writes = []
        original_popen = m.subprocess.Popen
        class ShortPipe:
            def __init__(self, pipe): self.pipe = pipe
            def write(self, block):
                writes.append(len(block))
                return self.pipe.write(block[:7])
            @property
            def closed(self): return self.pipe.closed
            def close(self): return self.pipe.close()
        def popen(*args, **kwargs):
            child = original_popen(*args, **kwargs)
            child.stdin = ShortPipe(child.stdin)
            return child
        with patch.object(m.subprocess, 'Popen', popen):
            result, receipt = self.blob_result(blob)
        self.assertEqual(result, 0, receipt)
        self.assertEqual(receipt['gzip_test_exit_code'], 0)
        self.assertGreater(len(writes), len(blob) // (64 * 1024))
        self.assertEqual(receipt['sha256'], hashlib.sha256(blob).hexdigest())

    def test_corrupt_deflate_crc_and_truncation_are_gzip_failures(self):
        blob = gzip.compress(b'-- PostgreSQL database dump complete\n')
        invalid_deflate = blob[:10] + b'\x07' + blob[-8:]
        corrupt_crc = blob[:-8] + bytes([blob[-8] ^ 1]) + blob[-7:]
        for damaged in [invalid_deflate, corrupt_crc, blob[:-3]]:
            result, receipt = self.blob_result(damaged)
            self.assertEqual(result, 1, receipt)
            self.assertEqual(receipt['stage'], 'gzip-test')
            self.assertEqual(receipt['generation'], '44')
            self.assertNotIn('sha256', receipt)

    def test_native_validator_refusal_cannot_be_overruled_by_python(self):
        original_popen = m.subprocess.Popen
        def popen(command, **kwargs):
            self.assertEqual(command, ['gzip', '-t'])
            return original_popen(['sh', '-c', 'cat >/dev/null; exit 23'], **kwargs)
        with patch.object(m.subprocess, 'Popen', popen):
            result, receipt = self.blob_result(gzip.compress(b'-- PostgreSQL database dump complete\n'))
        self.assertEqual(result, 1)
        self.assertEqual(receipt['stage'], 'gzip-test')
        self.assertEqual(receipt['gzip_test_exit_code'], 23)
        self.assertNotIn('completion_trailer', receipt)

    def test_chunked_trailer_preserves_whitespace_and_refuses_oversized_headers(self):
        marker = b'-- PostgreSQL database dump complete'
        for width in [1, 7, 64 * 1024]:
            body = b'  \\restrict Key123 \r\n' + b'SELECT ' + b'a' * (128 * 1024) + b';\n\t' + marker + b' ' * (128 * 1024) + b'\n--\n\\unrestrict Key123\r\n'
            trailer = m.CompletionTrailer()
            for start in range(0, len(body), width):
                trailer.feed(body[start:start + width])
                self.assertLessEqual(len(trailer.line), 4096)
                self.assertLessEqual(len(trailer.pending), 4096)
            self.assertTrue(trailer.valid())
        for body in [b'\\restrict ' + b'A' * 5000 + b'\n' + marker,
                     marker + b'\nSELECT ' + b'a' * (128 * 1024),
                     marker + b'a' * (128 * 1024)]:
            result, receipt = self.blob_result(gzip.compress(body))
            self.assertEqual(result, 1)
            self.assertEqual(receipt['stage'], 'completion-trailer')

    def test_extra_compressed_bytes_and_mismatched_length_are_refused(self):
        blob = gzip.compress(b'-- PostgreSQL database dump complete\n')
        for data, size in [(blob, len(blob) + 1), (blob, len(blob) - 1), (blob + b'garbage', len(blob) + 7)]:
            result, receipt = self.blob_result(data, size)
            self.assertEqual(result, 1)
            self.assertEqual(receipt['generation'], '44')
            self.assertNotIn('completion_trailer', receipt)

subprocess_run = m.subprocess.run

unittest.main()
"#;

#[test]
fn the_recorded_door_is_bounded_to_one_secret_and_no_packet_arguments() {
    let root = repo_root();
    let uploader =
        std::fs::read_to_string(root.join("infra/cluster/manifests/boss-backup.yaml")).unwrap();
    assert!(
        uploader.contains("STAMP=$(date -u +%Y%m%d-%H%M%S)"),
        "Update the real-object fixtures and exact readback filter when the uploader's stamp changes"
    );
    let verb: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("infra/ops/verbs/check-gcs-backup.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(verb["hosts"], serde_json::json!(["forge"]));
    assert!(
        verb["about"]
            .as_str()
            .unwrap()
            .contains("bounded streaming")
    );
    assert_eq!(verb["params"], serde_json::json!([]));
    assert_eq!(
        verb["argv"],
        serde_json::json!(["infra/forge/check-gcs-backup.sh"])
    );
    let script = std::fs::read_to_string(root.join("infra/forge/check-gcs-backup.sh")).unwrap();
    assert!(script.contains("get secret boss-gcs-offsite -n boss"));
    assert!(script.contains("stat -f -c %T /dev/shm"));
    assert!(script.contains("--read-only"));
    assert!(script.contains("trap cleanup EXIT"));
    for absent in ["kubectl exec", "storage cp", "storage rm", "-v "] {
        assert!(!script.contains(absent), "{absent}");
    }
    let rule =
        std::fs::read_to_string(root.join("infra/dispatcher/rules/check-gcs-backup-weekly.toml"))
            .unwrap();
    assert!(rule.contains("cadence = \"weekly\""));
    assert!(rule.contains("handler = \"jobs.spawn\""));
    let watch =
        std::fs::read_to_string(root.join("infra/dispatcher/rules/watch-check-gcs-backup.toml"))
            .unwrap();
    assert!(watch.contains("handler = \"ops.judge\""));
    assert!(watch.contains("remote archive integrity verified"));
}

#[test]
fn launcher_reads_only_the_named_secret_hides_values_and_cleans_its_container() {
    let dir = scratch_dir("gcs-launcher");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exec(
        &bin.join("sudo"),
        r#"#!/bin/bash
set -eu
printf '%s\n' "$*" >> "$STUB_LOG"
case "$*" in
  *'kubectl --kubeconfig=/kc get secret boss-gcs-offsite -n boss '*)
    [ "${STUB_FAIL:-}" != secret ] || { echo credential-marker >&2; exit 1; }
    echo '{"kind":"Secret","metadata":{"name":"boss-gcs-offsite","namespace":"boss"},"data":{"sa.json":"Y3JlZGVudGlhbC1tYXJrZXI=","bucket":"dGVzdC1idWNrZXQ="}}' ;;
  *'docker run --rm --name boss-gcs-readback-'*)
    [ "${STUB_FAIL:-}" != download ] || exit 1
    [ "${STUB_FAIL:-}" != silent ] || exit 0
    receipt='{"result":"ok","stage":"verified","claim":"remote-archive-integrity","generation":"44","object":"boss-20261002-091000.sql.gz","object_bytes":10,"downloaded_bytes":10,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","gzip_test":true,"gzip_test_exit_code":0,"completion_trailer":true,"verification_mode":"bounded-stream","listed_pages":2,"matching_dumps":2,"checked_at":"2026-10-02T02:00:00Z"}'
    case "${STUB_FAIL:-}" in
      legacy) receipt=$(jq -c 'del(.verification_mode)' <<< "$receipt") ;;
      gzip-exit) receipt=$(jq -c '.gzip_test_exit_code = 23' <<< "$receipt") ;;
    esac
    printf 'receipt: %s\n' "$receipt" ;;
  *'docker image inspect '*|*'docker container inspect '*|*'docker create '*|*'docker rm '*) exit 0 ;;
  *) echo "unexpected stub argv" >&2; exit 2 ;;
esac
"#,
    );
    let log = dir.join("argv");
    for mode in ["", "secret", "download", "silent", "legacy", "gzip-exit"] {
        write_file(&log, "");
        let output = Command::new("bash")
            .arg(repo_root().join("infra/forge/check-gcs-backup.sh"))
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("BOSS_SOR_ENV", dir.join("absent-sor.env"))
            .env("BOSS_FORGE_REGISTRY_HOST", "forge.test:3000")
            .env("STUB_LOG", &log)
            .env("STUB_FAIL", mode)
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            mode.is_empty(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let words = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !words.contains("credential-marker"),
            "Secret or provider error body was printed"
        );
        let calls = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            calls.matches("get secret boss-gcs-offsite -n boss").count(),
            1
        );
        if mode != "secret" {
            assert!(calls.contains("--read-only"));
            assert!(
                calls.contains("docker rm -f boss-gcs-readback-"),
                "owned container was not cleaned: {calls}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}
