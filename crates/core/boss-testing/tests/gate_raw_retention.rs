use boss_testing::{repo_root, scratch_dir};
use std::process::Command;

#[test]
fn failed_stdout_cannot_stamp_a_retained_receipt() {
    let dir = scratch_dir("raw-retention-sink");
    let script = r#"
import json, pathlib, subprocess, sys
helper, root = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
journal, receipt = root / 'journal.jsonl', root / 'receipt.json'
journal.write_bytes(b'x' * 100000)
receipt.write_text(json.dumps({'verdict':'green', 'runtime_evidence':{}}))
with open('/dev/full', 'wb') as sink:
    result = subprocess.run([sys.executable, str(helper), 'retain', str(journal), str(receipt)], stdout=sink, stderr=subprocess.PIPE)
assert result.returncode != 0
body = json.loads(receipt.read_bytes())
assert body['runtime_evidence'].get('raw_retention', {}).get('state') != 'retained', 'failed stdout stamped retained'
assert body['runtime_evidence']['raw_retention']['state'] == 'unavailable'
assert any('stdout retention failed' in value for value in body['runtime_evidence']['raw_retention']['diagnostics'])
assert body['verdict'] == 'green'
"#;
    let out = Command::new("python3")
        .args(["-c", script])
        .arg(repo_root().join("infra/gate-runner/runtime-evidence.py"))
        .arg(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// THE RUNNER RETAINS ITS RAW EVIDENCE EXACTLY ONCE, BEFORE IT DELIVERS
/// ITS VERDICT AND BEFORE IT EMPTIES ITS WORKSPACE — IN BOTH LAYOUTS.
///
/// Since backlog 934ccad1 a runner delivers its verdict one of two ways:
/// handed `GATE_VERDICT_CARRIER=pod-log` it LEAVES it (the receipt frame
/// in its log, the trailer in its termination message) and reports
/// nothing; handed nothing it reports to the packet as before. The
/// retention must precede either, on the normal path and on every early
/// exit, so each mode below runs under each layout — with the layout SET
/// by this test, never inherited.
///
/// INHERITED IS HOW THIS WENT RED ON GATE-RUN 5d576fba. The gate pod of a
/// tree whose manifest hands the runner the carrier hands it to every
/// process in the pod, this test's bash included: the lifted `fail_lost`
/// took the carrier arm, the stub `report` this test waited for never
/// ran, and the block wrote a `lost` trailer naming the REAL Job into the
/// REAL pod's termination message. Green on the dev pod, which has no
/// such variable. run.sh no longer exports the two variables to anything
/// it runs (the carrier block's `export -n`), and this test no longer
/// asks its environment which layout it is in.
#[test]
fn runner_retains_once_before_normal_report_and_lost_exit_cleanup() {
    let dir = scratch_dir("raw-retention-runner");
    let script = r#"
import json, os, pathlib, subprocess, sys
root, tree = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
source = (tree / 'infra/gate-runner/run.sh').read_text()
begin, end = '# --- raw-runtime retention (begin) ---', '# --- raw-runtime retention (end) ---'
assert begin in source and end in source, 'runner has no verdict-independent retention seam'
block = source.split(begin, 1)[1].split(end, 1)[0]
block = block.replace('/gate-target/repo/infra/gate-runner/runtime-evidence.py', str(tree / 'infra/gate-runner/runtime-evidence.py'))
assert source.index(begin) < source.index("trap 'fail_lost")
assert 'retain_runtime_raw; settle_early lost' in source
carrier = source.split('# --- verdict carrier (begin) ---', 1)[1].split('# --- verdict carrier (end) ---', 1)[0]
delivery = source.split('# --- verdict delivery (begin) ---', 1)[1].split('# --- verdict delivery (end) ---', 1)[0]
assert "trap 'retain_runtime_raw; empty_workspace' EXIT" in source
normal = source.index('retain_runtime_raw\n# THE VERDICT')
assert normal < source.index('python3 - "$RECEIPT" /gate-target/gate.log /gate-target/failed-checks.txt')
# The normal path retains before EITHER delivery: the one block that
# leaves the verdict or reports it comes after the retention.
assert normal < source.index('# --- verdict delivery (begin) ---')
inherited = {k: v for k, v in os.environ.items()
             if k not in ('GATE_VERDICT_CARRIER', 'GATE_TERMINATION_LOG', 'NATIVE_JOB_NAME')}
for layout in ('reports', 'pod-log'):
  for mode in ('normal', 'lost', 'exit'):
    tag = layout + '-' + mode
    journal, receipt, message = root / (tag + '.jsonl'), root / (tag + '.json'), root / (tag + '.termination')
    journal.write_bytes(b'{"middle":"original bytes"}\n')
    receipt.write_text(json.dumps({'verdict': 'lost', 'runtime_evidence': {}}))
    if message.exists(): message.unlink()
    shell = 'set -e\n' + block + '\n' + carrier + '\n'
    shell += 'empty_workspace() { rm -f "$BOSS_GATE_RUNTIME_EVIDENCE"; echo cleanup; }\nreport() { echo report; }\n'
    shell += next(line for line in source.splitlines() if line.startswith('fail_lost()')) + '\n'
    shell += next(line.strip() for line in source.splitlines() if "trap 'retain_runtime_raw; empty_workspace' EXIT" in line) + '\n'
    # The normal path: the retention, then run.sh's OWN delivery lines.
    if mode == 'normal': shell += 'VERDICT=green\nSUMMARY=\'{"verdict":"green","runtime_evidence":{}}\'\nGATE_RUN_JOB_ID=pkt-1\nretain_runtime_raw\n' + delivery + '\n'
    if mode == 'lost': shell += next(line for line in source.splitlines() if line.startswith("trap 'fail_lost")) + '\nfalse\n'
    env = dict(inherited, BOSS_GATE_RUNTIME_EVIDENCE=str(journal), RECEIPT=str(receipt),
               BOSS_RUNTIME_HELPER=str(tree / 'infra/gate-runner/runtime-evidence.py'),
               GATE_TERMINATION_LOG=str(message), NATIVE_JOB_NAME='gate-retention-00000')
    if layout == 'pod-log': env['GATE_VERDICT_CARRIER'] = 'pod-log'
    result = subprocess.run(['bash', '-c', shell], env=env, capture_output=True)
    out = result.stdout
    assert result.returncode == (1 if mode == 'lost' else 0), (tag, result.stderr)
    assert out.count(b'"kind":"end"') == 1, (tag, 'retained %d times, not once' % out.count(b'"kind":"end"'), out)
    assert b'gate-runtime-raw:' in out and b'cleanup' in out, (tag, out)
    assert out.index(b'gate-runtime-raw:') < out.index(b'cleanup'), (tag, 'emptied the workspace before retaining', out)
    # The delivery, by layout: the stub report when the runner reports,
    # the receipt frame - and NO report, and a termination message -
    # when it leaves its verdict.
    delivered = b'report' if layout == 'reports' else b'gate-runner: receipt '
    if mode != 'exit':
        assert delivered in out, (tag, 'the verdict was not delivered by this layout', out)
        assert out.index(b'gate-runtime-raw:') < out.index(delivered), (tag, 'delivered before retaining', out)
    if layout == 'pod-log':
        assert b'report' not in out, (tag, 'a carrier runner reported', out)
        assert message.exists() == (mode != 'exit'), (tag, 'termination message')
    else:
        assert not message.exists(), (tag, 'a reporting runner wrote a termination message')
    assert not journal.exists(), tag
"#;
    let out = Command::new("python3")
        .args(["-c", script])
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

#[test]
fn journal_admission_includes_its_newline_in_the_byte_bound() {
    let dir = scratch_dir("raw-retention-newline");
    let script = r#"
import importlib.util, json, pathlib, sys
spec = importlib.util.spec_from_file_location('collector', sys.argv[1])
collector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(collector)
journal = pathlib.Path(sys.argv[2]) / 'boundary.jsonl'
row = collector.sample('boundary')
size = len(json.dumps(row, separators=(',', ':')).encode())
journal.write_bytes(b'x' * (collector.JOURNAL_BYTES - size))
before = journal.read_bytes()
collector.append(journal, row)
assert journal.read_bytes() == before, 'newline crossed the journal byte bound'
assert journal.with_suffix('.jsonl.truncated').exists()
"#;
    let out = Command::new("python3")
        .args(["-c", script])
        .arg(repo_root().join("infra/gate-runner/runtime-evidence.py"))
        .arg(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn raw_retention_preserves_damage_and_names_missing_or_excess_bytes() {
    let dir = scratch_dir("raw-retention-damage");
    let script = r#"
import base64, hashlib, json, pathlib, subprocess, sys
helper, root = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
for label, raw, state in [('damaged', b'\xff\x00{"unfinished":', 'retained'), ('empty', b'', 'retained'), ('overflow', b'x' * (2 * 1024 * 1024 + 1), 'partial'), ('missing', None, 'unavailable')]:
    journal, receipt = root / (label + '.jsonl'), root / (label + '.json')
    if raw is not None: journal.write_bytes(raw)
    receipt.write_bytes(b'not JSON: preserve original receipt')
    result = subprocess.run([sys.executable, str(helper), 'retain', str(journal), str(receipt)], capture_output=True)
    assert result.returncode == 0, result.stderr
    frames = [json.loads(line.split(b': ', 1)[1]) for line in result.stdout.splitlines() if line.startswith(b'gate-runtime-raw: ')]
    end = frames[-1]
    assert end['kind'] == 'end' and end['state'] == state
    restored = b''.join(base64.b64decode(f['data'], validate=True) for f in frames if f['kind'] == 'chunk')
    assert restored == (raw or b'')[:2 * 1024 * 1024]
    assert end['bytes'] == len(restored)
    assert end['sha256'] == (hashlib.sha256(restored).hexdigest() if raw is not None else None)
    assert receipt.read_bytes() == b'not JSON: preserve original receipt'
    assert end['receipt_binding']['state'] == 'unavailable'
    if state != 'retained': assert end['diagnostics']
receipt.write_bytes(b' ' * (256 * 1024 + 1))
original = receipt.read_bytes()
result = subprocess.run([sys.executable, str(helper), 'retain', str(journal), str(receipt)], capture_output=True, check=True)
end = json.loads(result.stdout.splitlines()[-1].split(b': ', 1)[1])
assert 'reader byte bound' in end['receipt_binding']['reason']
assert receipt.read_bytes() == original
"#;
    let out = Command::new("python3")
        .args(["-c", script])
        .arg(repo_root().join("infra/gate-runner/runtime-evidence.py"))
        .arg(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn raw_journal_survives_every_receipt_verdict_and_workspace_cleanup() {
    let dir = scratch_dir("raw-retention");
    let script = r#"
import base64, hashlib, json, pathlib, subprocess, sys
helper = pathlib.Path(sys.argv[1])
root = pathlib.Path(sys.argv[2])
for verdict in ('green', 'failed', 'refused', 'lost'):
    journal = root / (verdict + '.jsonl')
    receipt = root / (verdict + '.json')
    subprocess.run([sys.executable, str(helper), 'sample', str(journal), 'first'], check=True, capture_output=True)
    sample = json.loads(journal.read_bytes())
    rows = []
    for index in range(80):
        row = dict(sample, label='phase-' + str(index), monotonic_ns=sample['monotonic_ns'] + index)
        rows.append(json.dumps(row).encode() + b'\n')
    raw = b''.join(rows)
    journal.write_bytes(raw)
    original = {'verdict': verdict, 'checks': [], 'fails': [], 'opaque': {'keep': True}}
    receipt.write_text(json.dumps(original))
    subprocess.run([sys.executable, str(helper), 'merge', str(journal), str(receipt)], check=True)
    before = json.loads(receipt.read_bytes())
    assert len(before['runtime_evidence']['samples']) <= 32
    assert 'phase-40' not in [row['label'] for row in before['runtime_evidence']['samples']]
    result = subprocess.run([sys.executable, str(helper), 'retain', str(journal), str(receipt)], capture_output=True)
    assert result.returncode == 0, result.stderr
    frames = [json.loads(line.split(b': ', 1)[1]) for line in result.stdout.splitlines() if line.startswith(b'gate-runtime-raw: ')]
    chunks = [frame for frame in frames if frame['kind'] == 'chunk']
    restored = b''.join(base64.b64decode(frame['data'], validate=True) for frame in chunks)
    end = frames[-1]
    assert end['kind'] == 'end' and end['bytes'] == len(raw)
    assert end['sha256'] == hashlib.sha256(raw).hexdigest()
    after = json.loads(receipt.read_bytes())
    for key, value in original.items(): assert after[key] == value
    retained = after['runtime_evidence']['raw_retention']
    assert retained['state'] == 'retained' and retained['sha256'] == end['sha256']
    journal.unlink()
    assert restored == raw and b'phase-40' in restored
"#;
    let out = Command::new("python3")
        .args(["-c", script])
        .arg(repo_root().join("infra/gate-runner/runtime-evidence.py"))
        .arg(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
