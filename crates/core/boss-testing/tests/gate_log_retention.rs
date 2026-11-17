//! GATE.LOG IS FRAMED WHOLE BEFORE ANYTHING REDUCES IT (backlog 7c49bd38;
//! design 5a274e7c car 1, decided 2026-11-13).
//!
//! The runner's failure-detail extractor keeps failure blocks, the last
//! 300 lines of each failed check and a 24,000-character excerpt, and
//! the EXIT trap then deletes gate.log with the workspace. That was the
//! only copy: train dc3f2ee7's outer log (1,434,667 B) held none of the
//! trace records its source emitted. So `runtime-evidence.py retain-log`
//! writes gate.log to runner stdout as ordered, hashed, gzipped frames
//! and a `gate-runner: log-end v1` trailer, and binds the same facts
//! into the receipt as `log_retention.gate_log`, BEFORE the extractor
//! runs. Car 2 (the conductor) verifies the frames and stores them;
//! these tests prove the frames reproduce the original byte for byte,
//! that over the 4 MiB gzip bound the retained bytes are a STATED
//! prefix of a stated whole, and that a missing log or a broken stdout
//! says so instead of claiming a retention it did not make.
use boss_testing::{repo_root, scratch_dir};
use std::process::Command;

/// The reader every case below uses: frames for one stream, in order,
/// each hash checked, then the gzip against the trailer, then the
/// gunzipped bytes against the trailer's kept hash. It is the
/// verification car 2 owes, written once so each case states only what
/// it is about.
const READER: &str = r#"
import base64, gzip, hashlib, json, os, pathlib, subprocess, sys
FRAME, TRAILER = b'gate-runner-log: ', b'gate-runner: log-end v1 '
def run(helper, log, receipt, sink=None, **extra):
    env = {k: v for k, v in os.environ.items()
           if k not in ('NATIVE_JOB_NAME', 'POD_UID', 'BOSS_GATE_SOURCE_HEAD')}
    env.update(NATIVE_JOB_NAME='gate-log-fixture-00001', POD_UID='pod-uid-fixture',
               BOSS_GATE_SOURCE_HEAD='a' * 40, **extra)
    return subprocess.run([sys.executable, str(helper), 'retain-log', str(log), str(receipt)],
                          env=env, stdout=sink if sink is not None else subprocess.PIPE,
                          stderr=subprocess.PIPE)
def trailer(out):
    ends = [l for l in out.splitlines() if l.startswith(TRAILER)]
    assert len(ends) == 1, ('one trailer, not %d' % len(ends), out[-2000:])
    head, _, reason = ends[0][len(TRAILER):].decode().partition(' reason=')
    fields = dict(f.split('=', 1) for f in head.split(' '))
    fields['reason'] = reason or None
    return fields
def restore(out):
    end = trailer(out)
    frames = [json.loads(l[len(FRAME):]) for l in out.splitlines() if l.startswith(FRAME)]
    assert out.index(FRAME) < out.index(TRAILER) if frames else True, 'trailer before a frame'
    assert all(f['stream'] == 'gate.log' for f in frames)
    assert [f['i'] for f in frames] == list(range(len(frames))), 'frames out of order'
    assert all(f['of'] == len(frames) for f in frames)
    parts = [base64.b64decode(f['b64'], validate=True) for f in frames]
    for f, p in zip(frames, parts):
        assert hashlib.sha256(p).hexdigest() == f['sha256'], ('frame hash', f['i'])
    packed = b''.join(parts)
    assert int(end['frames']) == len(frames)
    assert int(end['gz_bytes']) == len(packed)
    assert end['gz_sha256'] == hashlib.sha256(packed).hexdigest()
    kept = gzip.decompress(packed)
    assert int(end['kept_bytes']) == len(kept)
    assert end['kept_sha256'] == hashlib.sha256(kept).hexdigest()
    return end, packed, kept
"#;

fn python(script: &str, dir: &std::path::Path) {
    let out = Command::new("python3")
        .args(["-c", &format!("{READER}\n{script}")])
        .arg(repo_root().join("infra/gate-runner/runtime-evidence.py"))
        .arg(dir)
        .arg(repo_root())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The item's own acceptance: the sole cause sits in the MIDDLE, before
/// a 13,000-line passing tail - exactly where a tail-based reduction
/// loses it - and comes back byte-exact, non-UTF-8 and CRLF included.
#[test]
fn a_middle_failure_before_a_long_passing_tail_comes_back_byte_exact() {
    let dir = scratch_dir("gate-log-middle");
    python(
        r#"
helper, root = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
log, receipt = root / 'gate.log', root / 'receipt.json'
cause = b"thread 'ledger::the_middle_cause' panicked at crates/x/src/lib.rs:42:5:\n"
head = b'::group::gate: fmt\nok\n::endgroup::\n::group::gate: test\n'
trace = b'selection_trace {"phase":"middle","bytes":"\xff\xfe"}\r\n'
tail = b''.join(b'test passing::case_%05d ... ok\n' % n for n in range(13000))
original = head + cause + trace + b'test ledger::the_middle_cause ... FAILED\n' + tail + b'::endgroup::\n'
log.write_bytes(original)
receipt.write_text(json.dumps({'verdict': 'failed', 'checks': [{'name': 'test'}],
                               'log_retention': {'other': {'kept': True}}, 'opaque': [1, 2]}))
result = run(helper, log, receipt)
assert result.returncode == 0, result.stderr
end, packed, kept = restore(result.stdout)
assert kept == original, 'the frames did not reproduce gate.log byte for byte'
assert kept.index(cause) < kept.index(b'case_12999'), 'the middle cause is not in the middle'
assert end['state'] == 'retained' and end['reason'] is None
assert end['job'] == 'gate-log-fixture-00001' and end['stream'] == 'gate.log'
assert int(end['bytes']) == len(original)
assert end['sha256'] == hashlib.sha256(original).hexdigest() == end['kept_sha256']
body = json.loads(receipt.read_bytes())
assert body['verdict'] == 'failed' and body['opaque'] == [1, 2] and body['checks'] == [{'name': 'test'}]
assert body['log_retention']['other'] == {'kept': True}, 'another stream binding was dropped'
bound = body['log_retention']['gate_log']
assert bound['state'] == 'retained'
assert bound['original_bytes'] == len(original) and bound['original_sha256'] == end['sha256']
assert bound['gz_bytes'] == len(packed) and bound['gz_sha256'] == end['gz_sha256']
assert bound['frames'] == int(end['frames']) and bound['encoding'] == 'gzip'
assert bound['job'] == 'gate-log-fixture-00001' and bound['pod_uid'] == 'pod-uid-fixture'
assert bound['head'] == 'a' * 40
# The real per-gate size is on every receipt (the design marks it unmeasured).
assert bound['bound_gz_bytes'] == 4 * 1024 * 1024
"#,
        &dir,
    );
}

/// Over the bound nothing is dropped quietly: the frames hold a gzip of
/// the leading bytes, the trailer says `partial` with a reason, and the
/// byte count and hash are the WHOLE original's. A large log that
/// compresses under the bound - build chatter does, roughly tenfold -
/// is retained whole.
#[test]
fn an_over_bound_log_is_a_stated_prefix_of_a_stated_whole() {
    let dir = scratch_dir("gate-log-bound");
    python(
        r#"
helper, root = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
bound = 4 * 1024 * 1024
log, receipt = root / 'noise.log', root / 'noise.json'
original = os.urandom(6 * 1024 * 1024) + b'\nthe last line\n'
log.write_bytes(original)
receipt.write_text(json.dumps({'verdict': 'green'}))
result = run(helper, log, receipt)
assert result.returncode == 0, result.stderr
end, packed, kept = restore(result.stdout)
assert end['state'] == 'partial' and 'bound' in end['reason'], end
assert len(packed) <= bound, 'the gzip crossed its own bound'
assert 0 < len(kept) < len(original) and original.startswith(kept), 'not a prefix'
assert int(end['bytes']) == len(original), 'the whole was not stated'
assert end['sha256'] == hashlib.sha256(original).hexdigest()
bound_record = json.loads(receipt.read_bytes())['log_retention']['gate_log']
assert bound_record['state'] == 'partial' and bound_record['original_bytes'] == len(original)
assert bound_record['kept_bytes'] == len(kept) and 'bound' in bound_record['reason']
chatter, chatter_receipt = root / 'chatter.log', root / 'chatter.json'
lines = (b'   Compiling crate-%05d v0.1.%d (/gate-target/repo/crates/x)\n' % (n, n % 97)
         for n in range(400000))
whole = b''.join(lines)
assert len(whole) > 20 * 1024 * 1024
chatter.write_bytes(whole)
chatter_receipt.write_text(json.dumps({'verdict': 'green'}))
result = run(helper, chatter, chatter_receipt)
assert result.returncode == 0, result.stderr
end, packed, kept = restore(result.stdout)
assert end['state'] == 'retained' and kept == whole and len(packed) <= bound
"#,
        &dir,
    );
}

/// No gate.log is `unavailable` with its reason and no frame, never a
/// zero-byte retention. An unreadable receipt is left exactly as it
/// was - it may be the only copy - and the frames still go out.
#[test]
fn a_missing_log_is_unavailable_and_says_why() {
    let dir = scratch_dir("gate-log-missing");
    python(
        r#"
helper, root = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
receipt = root / 'receipt.json'
receipt.write_text(json.dumps({'verdict': 'lost'}))
result = run(helper, root / 'never-written.log', receipt)
assert result.returncode == 0, result.stderr
assert b'gate-runner-log: ' not in result.stdout
end = trailer(result.stdout)
assert end['state'] == 'unavailable' and end['frames'] == '0'
assert end['bytes'] == '-' and end['sha256'] == '-', 'absent evidence was written as a value'
assert 'never-written.log' in end['reason'], end
bound = json.loads(receipt.read_bytes())['log_retention']['gate_log']
assert bound['state'] == 'unavailable' and bound['original_sha256'] is None
assert bound['original_bytes'] is None and bound['reason'] == end['reason']
log, unreadable = root / 'gate.log', root / 'unreadable.json'
log.write_bytes(b'::group::gate: fmt\nok\n::endgroup::\n')
unreadable.write_bytes(b'not JSON: preserve the original receipt')
result = run(helper, log, unreadable)
assert result.returncode == 0, result.stderr
end, packed, kept = restore(result.stdout)
assert end['state'] == 'retained' and kept == log.read_bytes()
assert unreadable.read_bytes() == b'not JSON: preserve the original receipt'
"#,
        &dir,
    );
}

/// The receipt says `retained` only once stdout has taken the frames: a
/// sink that refuses them leaves `unavailable` with the reason, a
/// nonzero exit, and the verdict untouched.
#[test]
fn a_failed_stdout_never_stamps_a_retained_log() {
    let dir = scratch_dir("gate-log-sink");
    python(
        r#"
helper, root = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
log, receipt = root / 'gate.log', root / 'receipt.json'
log.write_bytes(b'x' * 100000)
receipt.write_text(json.dumps({'verdict': 'green'}))
with open('/dev/full', 'wb') as sink:
    result = run(helper, log, receipt, sink=sink)
assert result.returncode != 0
body = json.loads(receipt.read_bytes())
assert body['verdict'] == 'green'
assert body['log_retention']['gate_log']['state'] == 'unavailable'
assert 'stdout' in body['log_retention']['gate_log']['reason']
"#,
        &dir,
    );
}

/// THE ORDER IS THE POINT. run.sh frames gate.log after gate.sh has
/// exited and before the extractor reduces it, before the verdict is
/// left or reported (so the receipt the conductor records already
/// names the log's hash), and on the normal path before the EXIT trap
/// empties the workspace. The block itself, lifted from run.sh, frames
/// a real log, and answers a collector that is absent or too old to
/// know the action with an `unavailable` trailer - never a failure of
/// the run, which has its verdict to deliver.
#[test]
fn the_runner_frames_gate_log_before_the_extractor_and_the_delivery() {
    let dir = scratch_dir("gate-log-runner");
    python(
        r#"
helper, root, tree = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), pathlib.Path(sys.argv[3])
source = (tree / 'infra/gate-runner/run.sh').read_text()
begin, finish = '# --- gate-log retention (begin) ---', '# --- gate-log retention (end) ---'
assert begin in source and finish in source, 'run.sh has no gate-log retention block'
block = source.split(begin, 1)[1].split(finish, 1)[0]
call = '\nretain_gate_log /gate-target/gate.log "$RECEIPT"\n'
assert source.count(call) == 1, 'gate.log is framed exactly once, on the normal path'
at = source.index(call)
assert source.index('./infra/gate.sh ${GATE_MODE:-} > /gate-target/gate.log') < at, 'framed before gate.sh ran'
assert at < source.index('python3 - "$RECEIPT" /gate-target/gate.log /gate-target/failed-checks.txt'), 'the extractor reduces gate.log before it is framed'
assert at < source.index('# --- verdict delivery (begin) ---'), 'the verdict is delivered before gate.log is framed'
assert at < source.index('retain_runtime_raw\n# THE VERDICT')
log, receipt = root / 'gate.log', root / 'receipt.json'
log.write_bytes(b'::group::gate: test\n' + b'test a ... ok\n' * 5000 + b'::endgroup::\n')
old = root / 'old-collector.py'
old.write_text('raise SystemExit("unknown runtime evidence action")\n')
for label, collector in (('current', helper), ('absent', root / 'absent.py'), ('old', old)):
    receipt.write_text(json.dumps({'verdict': 'green'}))
    shell = 'set -euo pipefail\nRECEIPT=' + str(receipt) + '\n'
    shell += block.replace('/gate-target/repo/infra/gate-runner/runtime-evidence.py', str(collector))
    shell += call.replace('/gate-target/gate.log', str(log)) + 'echo the-runner-lived\n'
    env = {k: v for k, v in os.environ.items() if k != 'NATIVE_JOB_NAME'}
    env['NATIVE_JOB_NAME'] = 'gate-log-fixture-00002'
    result = subprocess.run(['bash', '-c', shell], env=env, capture_output=True)
    assert result.returncode == 0, (label, result.stderr)
    assert result.stdout.rstrip().endswith(b'the-runner-lived'), (label, result.stdout[-500:])
    end = trailer(result.stdout)
    assert end['job'] == 'gate-log-fixture-00002', (label, end)
    if label == 'current':
        end, packed, kept = restore(result.stdout)
        assert end['state'] == 'retained' and kept == log.read_bytes()
        assert json.loads(receipt.read_bytes())['log_retention']['gate_log']['state'] == 'retained'
    else:
        assert end['state'] == 'unavailable' and end['reason'], (label, end)
        assert b'gate-runner-log: ' not in result.stdout, label
"#,
        &dir,
    );
}
