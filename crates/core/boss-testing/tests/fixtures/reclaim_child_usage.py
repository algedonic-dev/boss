import importlib.util
import json
import os
from pathlib import Path
import sys

spec = importlib.util.spec_from_file_location('observer', sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
root = Path(sys.argv[2])
root.mkdir(mode=0o700, parents=True, exist_ok=True)
root.chmod(0o700)
bin_path = root / 'bin'
bin_path.mkdir(mode=0o700)
git = bin_path / 'git'
git.write_text("#!/usr/bin/env python3\nimport sys\nassert sys.argv[1:]==['status','SYNTHETIC_PRIVATE_MARKER']\nallocation=bytearray(24*1024*1024)\nprint('original stdout')\nprint('original stderr',file=sys.stderr)\nsys.exit(7)\n")
git.chmod(0o700)
os.environ['PATH'] = str(bin_path) + ':' + os.environ['PATH']
os.environ['BOSS_RECLAIM_GIT_DIR'] = str(root / 'ledger')
assert m.run('fixture', ['status', 'SYNTHETIC_PRIVATE_MARKER']) == 7
entry = m.records()[0]
begin = m.read_record(entry, 'begin.json')
end = m.read_record(entry, 'end.json')
assert begin['schema'] == end['schema'] == 2, 'native demand needs a versioned record'
assert m.valid_begin(begin) and m.valid_end(end, begin)
usage = end['child_usage']
assert set(usage) == {'scope', 'max_rss_kib', 'user_cpu_us', 'system_cpu_us'}
assert usage['scope'] == 'wait4 native child'
assert type(usage['max_rss_kib']) is int and usage['max_rss_kib'] >= 24 * 1024
assert all(type(usage[k]) is int and usage[k] >= 0 for k in ['user_cpu_us', 'system_cpu_us'])
assert end['pid'] == begin['pid'] and end['exit_code'] == 7
for row in [begin, end]:
    assert {'slab_reclaimable', 'slab_unreclaimable'} <= set(row['accounting'])
assert 'SYNTHETIC_PRIVATE_MARKER' not in json.dumps([begin, end])
for field in ['max_rss_kib', 'user_cpu_us', 'system_cpu_us']:
    for value in [True, 1.0, -1, 'private value']:
        changed = {**end, 'child_usage': {**usage, field: value}}
        assert not m.valid_end(changed, begin), (field, value)
        m.write_record(entry, 'end.json', changed)
        assert m.state()['state'] == 'unknown'
        phase = m.snapshot(1)['phases'][0]
        assert phase['status'] == 'unknown_terminal' and 'terminal' not in phase
        before = {name: (entry / name).read_bytes() for name in ['begin.json', 'end.json']}
        m.MAX_RECORDS = 1
        try:
            m.reserve()
        except ValueError:
            pass
        else:
            raise AssertionError('untyped child demand was retired')
        assert all((entry / name).read_bytes() == value for name, value in before.items())
        m.MAX_RECORDS = 256
m.write_record(entry, 'end.json', end)
legacy = {'schema': 1, 'phase': 'fixture', 'operation': 'status', 'pid': 1, 'start_ticks': 1, 'boot_id': m.boot_id(), 'cgroup_ns': 1, 'at_ns': 1, 'accounting_scope': 'observer own cgroup', 'accounting': m.accounting()}
legacy_end = {'schema': 1, 'pid': 1, 'exit_code': 0, 'observed': True, 'at_ns': 2, 'accounting': m.accounting()}
assert m.valid_begin(legacy) and m.valid_end(legacy_end, legacy)
assert not m.valid_end({**end, 'schema': 1}, begin)
assert not m.valid_end({**legacy_end, 'schema': 2}, legacy)
archive = m.snapshot(256)
assert archive['phases'][0]['terminal']['child_usage'] == usage
print('native child high-water separated from cgroup categories; typed and legacy evidence conserved')
