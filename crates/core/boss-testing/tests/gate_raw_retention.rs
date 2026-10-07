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

#[test]
fn runner_retains_once_before_normal_report_and_lost_exit_cleanup() {
    let dir = scratch_dir("raw-retention-runner");
    let script = r#"
import json, pathlib, subprocess, sys
root, tree = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
source = (tree / 'infra/gate-runner/run.sh').read_text()
begin, end = '# --- raw-runtime retention (begin) ---', '# --- raw-runtime retention (end) ---'
assert begin in source and end in source, 'runner has no verdict-independent retention seam'
block = source.split(begin, 1)[1].split(end, 1)[0]
block = block.replace('/gate-target/repo/infra/gate-runner/runtime-evidence.py', str(tree / 'infra/gate-runner/runtime-evidence.py'))
assert source.index(begin) < source.index("trap 'fail_lost")
assert 'retain_runtime_raw; report lost' in source
assert "trap 'retain_runtime_raw; empty_workspace' EXIT" in source
normal = source.index('retain_runtime_raw\n# THE VERDICT')
assert normal < source.index('python3 - "$RECEIPT" /gate-target/gate.log /gate-target/failed-checks.txt')
for mode in ('normal', 'lost', 'exit'):
    journal, receipt, trace = root / (mode + '.jsonl'), root / (mode + '.json'), root / (mode + '.trace')
    journal.write_bytes(b'{"middle":"original bytes"}\n')
    receipt.write_text(json.dumps({'verdict': 'lost', 'runtime_evidence': {}}))
    shell = 'set -e\n' + block + '\n'
    shell += 'empty_workspace() { rm -f "$BOSS_GATE_RUNTIME_EVIDENCE"; echo cleanup; }\nreport() { echo report; }\n'
    shell += next(line for line in source.splitlines() if line.startswith('fail_lost()')) + '\n'
    shell += next(line.strip() for line in source.splitlines() if "trap 'retain_runtime_raw; empty_workspace' EXIT" in line) + '\n'
    if mode == 'normal': shell += 'retain_runtime_raw\necho report\n'
    if mode == 'lost': shell += next(line for line in source.splitlines() if line.startswith("trap 'fail_lost")) + '\nfalse\n'
    result = subprocess.run(['bash', '-c', shell], env=dict(__import__('os').environ,
        BOSS_GATE_RUNTIME_EVIDENCE=str(journal), RECEIPT=str(receipt), BOSS_RUNTIME_HELPER=str(tree / 'infra/gate-runner/runtime-evidence.py')), capture_output=True)
    assert result.returncode == (1 if mode == 'lost' else 0), result.stderr
    assert result.stdout.count(b'"kind":"end"') == 1, result.stdout
    assert result.stdout.index(b'gate-runtime-raw:') < result.stdout.index(b'cleanup')
    if mode != 'exit': assert result.stdout.index(b'gate-runtime-raw:') < result.stdout.index(b'report')
    assert not journal.exists()
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
