//! Reclaimer Git reads leave durable, value-free child evidence before execution.
use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::process::Command;

#[test]
fn native_child_high_water_is_typed_and_separate_from_cgroup_accounting() {
    let root = scratch_dir("reclaim-child-demand");
    let output = Command::new("python3")
        .arg(repo_root().join("crates/core/boss-testing/tests/fixtures/reclaim_child_usage.py"))
        .arg(repo_root().join("infra/cluster/dev-reclaim-git.py"))
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("original stdout\n"));
    assert_eq!(output.stderr, b"original stderr\n");
}

#[test]
fn directory_creation_refuses_a_linked_ancestor_without_changing_its_target() {
    let root = scratch_dir("reclaim-directory-create-refusal");
    let output = Command::new("python3")
        .arg("-c")
        .arg(
            r#"import importlib.util,os,stat,sys
from pathlib import Path
spec=importlib.util.spec_from_file_location('observer',sys.argv[1])
observer=importlib.util.module_from_spec(spec); spec.loader.exec_module(observer)
root=Path(sys.argv[2])
safe=root/'safe'/'nested'
fd=observer.checked_directory(safe,create=True)
try:
 assert stat.S_ISDIR(os.fstat(fd).st_mode)
 assert os.fstat(fd).st_uid==os.getuid()
 assert stat.S_IMODE(os.fstat(fd).st_mode)==0o700
finally: os.close(fd)
target=root/'target'; target.mkdir()
(target/'conserved').write_bytes(b'original bytes\n')
linked=root/'linked'; linked.symlink_to(target,target_is_directory=True)
before=sorted(p.name for p in target.iterdir())
try:
 fd=observer.checked_directory(linked/'new-child',create=True)
except (OSError,ValueError): pass
else:
 os.close(fd)
 raise AssertionError('linked ancestor was accepted')
after=sorted(p.name for p in target.iterdir())
assert after==before, f'refusal changed its target: before={before}, after={after}'
assert (target/'conserved').read_bytes()==b'original bytes\n'
print('private creation passed; linked-ancestor refusal conserved its target')
"#,
        )
        .arg(repo_root().join("infra/cluster/dev-reclaim-git.py"))
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn reservation_creates_records_relative_to_its_open_directory() {
    let root = scratch_dir("reclaim-reserve-directory-binding");
    let output = Command::new("python3")
        .arg("-c")
        .arg(
            r#"import importlib.util,os,sys
from pathlib import Path
spec=importlib.util.spec_from_file_location('observer',sys.argv[1])
observer=importlib.util.module_from_spec(spec); spec.loader.exec_module(observer)
root=Path(sys.argv[2]); ledger=root/'ledger'; moved=root/'original-ledger'
target=root/'target'; target.mkdir()
(target/'conserved').write_bytes(b'original bytes\n')
os.environ['BOSS_RECLAIM_GIT_DIR']=str(ledger)
read=observer.read_record
def replace_name_after_read(path,name):
 try: return read(path,name)
 finally:
  if path==ledger and name=='retention.json' and not moved.exists():
   ledger.rename(moved)
   ledger.symlink_to(target,target_is_directory=True)
observer.read_record=replace_name_after_read
entry=observer.reserve()
assert sorted(p.name for p in target.iterdir())==['conserved'], 'record creation used a changed full path'
assert (target/'conserved').read_bytes()==b'original bytes\n'
assert (moved/entry.name).is_dir(), 'record belongs to the directory already opened'
assert observer.state()['state']=='unknown', 'changed path cannot be reported as observed'
print('reservation retained its opened directory; changed name is unknown')
"#,
        )
        .arg(repo_root().join("infra/cluster/dev-reclaim-git.py"))
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn retirement_refuses_a_changed_entry_without_removing_its_targets_bytes() {
    let root = scratch_dir("reclaim-retirement-directory-binding");
    let output = Command::new("python3")
        .arg("-c")
        .arg(r#"import importlib.util,os,sys
from pathlib import Path
spec=importlib.util.spec_from_file_location('observer',sys.argv[1])
observer=importlib.util.module_from_spec(spec); spec.loader.exec_module(observer)
root=Path(sys.argv[2]); ledger=root/'ledger'; moved=root/'original-entry'
target=root/'target'; target.mkdir()
for name in ['begin.json','end.json']: (target/name).write_bytes(b'conserved target bytes\n')
os.environ['BOSS_RECLAIM_GIT_DIR']=str(ledger)
entry=observer.reserve()
begin={'schema':1,'phase':'fixture','operation':'status','pid':1,'start_ticks':1,'boot_id':observer.boot_id(),'cgroup_ns':1,'at_ns':1,'accounting_scope':'observer own cgroup','accounting':observer.accounting()}
end={'schema':1,'pid':1,'exit_code':0,'observed':True,'at_ns':2,'accounting':observer.accounting()}
observer.write_record(entry,'begin.json',begin); observer.write_record(entry,'end.json',end)
observer.MAX_RECORDS=1
write=observer.write_record
def replace_entry_after_archive(path,name,row):
 write(path,name,row)
 if path==ledger and name=='retirement.json':
  entry.rename(moved)
  entry.symlink_to(target,target_is_directory=True)
observer.write_record=replace_entry_after_archive
try: observer.reserve()
except (OSError,ValueError): pass
else: raise AssertionError('changed retirement entry must be refused')
assert sorted(p.name for p in target.iterdir())==['begin.json','end.json'], 'retirement removed bytes through a changed path'
for name in ['begin.json','end.json']: assert (target/name).read_bytes()==b'conserved target bytes\n'
assert observer.read_record(ledger,'retirement.json')['begin']==begin
assert observer.read_record(moved,'begin.json')==begin
assert observer.read_record(moved,'end.json')==end
assert observer.state()['state']=='unknown'
print('changed retirement entry refused; target and original evidence conserved')
"#)
        .arg(repo_root().join("infra/cluster/dev-reclaim-git.py"))
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn function(source: &str, name: &str) -> Option<String> {
    let start = source.find(&format!("{name}() {{"))?;
    let end = source[start..].find("\n}\n")? + start + 3;
    Some(source[start..end].to_owned())
}

#[test]
fn owning_landing_reader_records_child_incarnation_before_git_exec() {
    let root = scratch_dir("reclaim-git-before-exec");
    let bin = root.join("bin");
    create_dir(&bin);
    let ledger = root.join("ledger");
    create_dir(&ledger);
    let checked = root.join("checked");
    write_exec(
        &bin.join("git"),
        r#"#!/usr/bin/env python3
import json,os,pathlib,sys
ledger=pathlib.Path(os.environ['BOSS_RECLAIM_GIT_DIR'])
begins=list(ledger.glob('*/begin.json'))
matches=[]
for path in begins:
 row=json.loads(path.read_text())
 if row.get('pid')==os.getpid(): matches.append(row)
if len(matches)!=1:
 pathlib.Path(os.environ['CHECKED']+'.failure').write_text('missing durable phase before Git execution; matching child records='+str(len(matches)))
 raise AssertionError('missing durable phase before Git execution')
row=matches[0]
assert row['phase']=='landed_by_content'
assert row['start_ticks']==int(pathlib.Path('/proc/self/stat').read_text().rsplit(')',1)[1].split()[19])
with pathlib.Path(os.environ['CHECKED']).open('a') as out: out.write('checked\n')
args=sys.argv[1:]
if 'merge-base' in args: print('fixture-base')
elif 'diff' in args and '--name-only' in args: sys.stdout.buffer.write(b'fixture-file\0')
elif 'diff' in args and '--quiet' in args: pass
else: raise AssertionError('unexpected fixture operation')
"#,
    );
    let source =
        std::fs::read_to_string(repo_root().join("infra/cluster/dev-scratch-reclaim.sh")).unwrap();
    let observer = repo_root().join("infra/cluster/dev-reclaim-git.sh");
    let wrapper = function(&source, "git").unwrap_or_default();
    let landing = function(&source, "landed_by_content").unwrap();
    let script = root.join("reader.sh");
    write_file(
        &script,
        &format!(
            "set -euo pipefail\nREPO_DIR=fixture\nRECLAIM_GIT_OBSERVER={}\n{}\n{}\nlanded_by_content fixture-head fixture-main\n",
            observer.display(),
            wrapper,
            landing
        ),
    );
    let output = Command::new("bash")
        .arg(&script)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("BOSS_RECLAIM_GIT_DIR", &ledger)
        .env("CHECKED", &checked)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}\nchild evidence={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        std::fs::read_to_string(root.join("checked.failure")).unwrap_or_default()
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "content\n");
    assert_eq!(std::fs::read_to_string(checked).unwrap().lines().count(), 3);
}

#[test]
fn value_free_observer_preserves_original_arguments_streams_and_nonzero_status() {
    let root = scratch_dir("reclaim-git-streams");
    let bin = root.join("bin");
    create_dir(&bin);
    let ledger = root.join("ledger");
    write_exec(
        &bin.join("git"),
        r#"#!/usr/bin/env python3
import sys
assert sys.argv[1:]==['-c','credential.fixture=SYNTHETIC_PRIVATE_MARKER','status','--porcelain']
sys.stdout.write('original stdout\n')
sys.stderr.write('original stderr\n')
sys.exit(7)
"#,
    );
    let output = Command::new("bash")
        .arg(repo_root().join("infra/cluster/dev-reclaim-git.sh"))
        .args([
            "--run",
            "fixture",
            "--",
            "-c",
            "credential.fixture=SYNTHETIC_PRIVATE_MARKER",
            "status",
            "--porcelain",
        ])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("BOSS_RECLAIM_GIT_DIR", &ledger)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"original stdout\n");
    assert_eq!(output.stderr, b"original stderr\n");
    for entry in std::fs::read_dir(ledger).unwrap() {
        let entry = entry.unwrap().path();
        if !entry.is_dir() {
            continue;
        }
        for name in ["begin.json", "end.json"] {
            let text = std::fs::read_to_string(entry.join(name)).unwrap();
            assert!(!text.contains("SYNTHETIC_PRIVATE_MARKER"));
            assert!(!text.contains("credential.fixture"));
        }
    }
}

#[test]
fn finite_retention_keeps_the_new_child_evidence_and_counts_retired_complete_rows() {
    let root = scratch_dir("reclaim-git-bound");
    let bin = root.join("bin");
    create_dir(&bin);
    let ledger = root.join("ledger");
    write_exec(
        &bin.join("git"),
        "#!/usr/bin/env python3\nimport os,pathlib\npathlib.Path(os.environ['LAST_PID']).write_text(str(os.getpid()))\nprint('unchanged')\n",
    );
    let run = || {
        Command::new("bash")
            .arg(repo_root().join("infra/cluster/dev-reclaim-git.sh"))
            .args(["--run", "fixture", "--", "status"])
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("BOSS_RECLAIM_GIT_DIR", &ledger)
            .env("LAST_PID", root.join("last-pid"))
            .output()
            .unwrap()
    };
    assert!(run().status.success());
    let template = std::fs::read_dir(&ledger)
        .unwrap()
        .find_map(|x| {
            let path = x.unwrap().path();
            path.is_dir().then_some(path)
        })
        .unwrap();
    let source =
        std::fs::read_to_string(repo_root().join("infra/cluster/dev-reclaim-git.py")).unwrap();
    let bound: usize = source
        .lines()
        .find_map(|line| line.strip_prefix("MAX_RECORDS = "))
        .unwrap()
        .parse()
        .unwrap();
    for number in 1..bound {
        let entry = ledger.join(format!("{number:032x}"));
        create_dir(&entry);
        for name in ["begin.json", "end.json"] {
            std::fs::copy(template.join(name), entry.join(name)).unwrap();
        }
    }
    let output = run();
    assert!(
        output.status.success(),
        "the observer cannot change the Git decision"
    );
    assert_eq!(output.stdout, b"unchanged\n");
    let state = Command::new("bash")
        .arg(repo_root().join("infra/cluster/dev-reclaim-git.sh"))
        .arg("--state")
        .env("BOSS_RECLAIM_GIT_DIR", &ledger)
        .output()
        .unwrap();
    let row: serde_json::Value = serde_json::from_slice(&state.stdout).unwrap();
    let last_pid: u64 = std::fs::read_to_string(root.join("last-pid"))
        .unwrap()
        .parse()
        .unwrap();
    let matching = std::fs::read_dir(&ledger)
        .unwrap()
        .filter_map(|x| {
            let path = x.unwrap().path().join("begin.json");
            let bytes = std::fs::read(path).ok()?;
            let record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            (record["pid"] == last_pid).then_some(record)
        })
        .count();
    assert_eq!(
        matching, 1,
        "an unrecorded child cannot leave an observer PASS: {row}"
    );
    assert_eq!(row["state"], "observed");
    assert_eq!(row["dropped_complete"], 1);
}

#[test]
fn a_terminal_for_another_child_cannot_validate_the_begin_record() {
    let root = scratch_dir("reclaim-git-wrong-terminal");
    let bin = root.join("bin");
    create_dir(&bin);
    write_exec(&bin.join("git"), "#!/bin/sh\nexit 0\n");
    let ledger = root.join("ledger");
    let helper = repo_root().join("infra/cluster/dev-reclaim-git.sh");
    assert!(
        Command::new("bash")
            .arg(&helper)
            .args(["--run", "fixture", "--", "status"])
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
            )
            .env("BOSS_RECLAIM_GIT_DIR", &ledger)
            .status()
            .unwrap()
            .success()
    );
    let entry = std::fs::read_dir(&ledger)
        .unwrap()
        .find_map(|x| {
            let path = x.unwrap().path();
            path.is_dir().then_some(path)
        })
        .unwrap();
    let path = entry.join("end.json");
    let mut row: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    row["pid"] = serde_json::json!(row["pid"].as_u64().unwrap() + 1);
    std::fs::write(path, serde_json::to_vec(&row).unwrap()).unwrap();
    let output = Command::new("bash")
        .arg(helper)
        .arg("--state")
        .env("BOSS_RECLAIM_GIT_DIR", ledger)
        .output()
        .unwrap();
    let state: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        state["state"], "unknown",
        "mismatched child evidence: {state}"
    );
}

#[test]
fn malformed_phase_data_is_unknown_and_never_exported() {
    let root = scratch_dir("reclaim-git-malformed-phase");
    let bin = root.join("bin");
    create_dir(&bin);
    write_exec(&bin.join("git"), "#!/bin/sh\nexit 0\n");
    let ledger = root.join("ledger");
    let helper = repo_root().join("infra/cluster/dev-reclaim-git.sh");
    assert!(
        Command::new("bash")
            .arg(&helper)
            .args(["--run", "fixture", "--", "status"])
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
            )
            .env("BOSS_RECLAIM_GIT_DIR", &ledger)
            .status()
            .unwrap()
            .success()
    );
    let entry = std::fs::read_dir(&ledger)
        .unwrap()
        .find_map(|x| {
            let path = x.unwrap().path();
            path.is_dir().then_some(path)
        })
        .unwrap();
    let path = entry.join("begin.json");
    let mut row: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    row["phase"] = serde_json::json!("credential=SYNTHETIC_PRIVATE_MARKER");
    std::fs::write(path, serde_json::to_vec(&row).unwrap()).unwrap();
    let output = Command::new("bash")
        .arg(helper)
        .arg("--state")
        .env("BOSS_RECLAIM_GIT_DIR", ledger)
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&output.stdout).contains("SYNTHETIC_PRIVATE_MARKER"));
    let state: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(state["state"], "unknown", "malformed phase: {state}");
}

#[test]
fn untyped_operation_and_retention_data_return_loud_unknown() {
    let root = scratch_dir("reclaim-git-untyped");
    let helper = repo_root().join("infra/cluster/dev-reclaim-git.py");
    let output = Command::new("python3").args(["-c", r#"
import importlib.util,json,os,pathlib,sys
spec=importlib.util.spec_from_file_location('observer',sys.argv[1]); m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
root=pathlib.Path(sys.argv[2]); root.mkdir(mode=0o700); os.environ['BOSS_RECLAIM_GIT_DIR']=str(root)
entry=m.reserve()
begin={'schema':1,'phase':'fixture','operation':['SYNTHETIC_PRIVATE_MARKER'],'pid':1,'start_ticks':1,'boot_id':m.boot_id(),'cgroup_ns':1,'at_ns':1,'accounting_scope':'observer own cgroup','accounting':m.accounting()}
m.write_record(entry,'begin.json',begin)
assert m.state()['state']=='unknown'
begin['operation']='status'; m.write_record(entry,'begin.json',begin)
m.write_record(root,'retention.json',['SYNTHETIC_PRIVATE_MARKER'])
assert m.state()['state']=='unknown'
print('untyped evidence is unknown')
"#]).arg(helper).arg(root.join("ledger")).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"untyped evidence is unknown\n");
}

#[test]
fn an_unreaped_dead_child_is_unknown_and_unsafe_storage_preserves_git_outcome() {
    let root = scratch_dir("reclaim-git-interrupted");
    let helper = repo_root().join("infra/cluster/dev-reclaim-git.py");
    let output = Command::new("python3").args(["-c", r#"
import importlib.util,json,os,pathlib,sys,time
spec=importlib.util.spec_from_file_location('observer',sys.argv[1]); m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
root=pathlib.Path(sys.argv[2]); root.mkdir(mode=0o700); os.environ['BOSS_RECLAIM_GIT_DIR']=str(root)
entry=m.reserve(); child=os.fork()
if child==0: os._exit(0)
try:
 deadline=time.monotonic()+5
 while time.monotonic()<deadline:
  fields=pathlib.Path('/proc/'+str(child)+'/stat').read_text().rsplit(')',1)[1].split()
  if fields[0]=='Z': break
  time.sleep(.01)
 else: raise AssertionError('fixture child did not reach zombie state')
 m.write_record(entry,'begin.json',{'schema':1,'phase':'fixture','operation':'status','pid':child,'start_ticks':m.start_ticks(child),'boot_id':m.boot_id(),'cgroup_ns':os.stat('/proc/self/ns/cgroup').st_ino,'at_ns':time.time_ns(),'accounting_scope':'observer own cgroup','accounting':m.accounting()})
 assert m.state()['state']=='unknown', m.state()
finally: os.waitpid(child,0)
print('dead incarnation is unknown')
"#]).arg(helper).arg(root.join("ledger")).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bin = root.join("bin");
    create_dir(&bin);
    write_exec(
        &bin.join("git"),
        "#!/bin/sh\nprintf 'original output\\n'\nexit 7\n",
    );
    let target = root.join("sentinel");
    create_dir(&target);
    write_file(&target.join("conserved"), "unchanged");
    std::os::unix::fs::symlink(&target, root.join("unsafe")).unwrap();
    let result = Command::new("bash")
        .arg(repo_root().join("infra/cluster/dev-reclaim-git.sh"))
        .args(["--run", "fixture", "--", "status"])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("BOSS_RECLAIM_GIT_DIR", root.join("unsafe"))
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(7));
    assert_eq!(result.stdout, b"original output\n");
    assert!(String::from_utf8_lossy(&result.stderr).contains("unknown storage"));
    assert_eq!(
        std::fs::read_to_string(target.join("conserved")).unwrap(),
        "unchanged"
    );
    assert_eq!(std::fs::read_dir(target).unwrap().count(), 1);
}

#[test]
fn snapshot_is_bounded_value_free_and_keeps_interrupted_evidence() {
    let root = scratch_dir("reclaim-git-snapshot");
    let helper = repo_root().join("infra/cluster/dev-reclaim-git.py");
    let output = Command::new("python3").args(["-c", r#"
import importlib.util,json,os,pathlib,sys,time
spec=importlib.util.spec_from_file_location('observer',sys.argv[1]); m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
root=pathlib.Path(sys.argv[2]); root.mkdir(mode=0o700); os.environ['BOSS_RECLAIM_GIT_DIR']=str(root)
for n in range(40):
 entry=m.reserve()
 row={'schema':1,'phase':'fixture','operation':'status','pid':99999999,'start_ticks':1,'boot_id':m.boot_id(),'cgroup_ns':os.stat('/proc/self/ns/cgroup').st_ino,'at_ns':n+1,'accounting_scope':'observer own cgroup','accounting':m.accounting()}
 m.write_record(entry,'begin.json',row)
 if n: m.write_record(entry,'end.json',{'schema':1,'pid':row['pid'],'exit_code':0,'observed':True,'at_ns':n+2,'accounting':m.accounting()})
sample=m.snapshot(32); full=m.snapshot(m.MAX_RECORDS)
assert sample['state']=='unknown' and sample['omitted']==8 and len(sample['phases'])==32, sample
assert any(x['status']=='interrupted_or_replaced' for x in sample['phases'])
assert full['omitted']==0 and len(full['phases'])==40
assert all(x['binding']=='current_observer_namespace' for x in full['phases'])
print('bounded sample and full safe archive agree')
"#]).arg(helper).arg(root.join("ledger")).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"bounded sample and full safe archive agree\n"
    );
}

#[test]
fn retirement_failure_cannot_silently_erase_observation_history() {
    let root = scratch_dir("reclaim-git-retirement-failure");
    let output = Command::new("python3").args(["-c", r#"
import importlib.util,json,os,pathlib,sys
spec=importlib.util.spec_from_file_location('observer',sys.argv[1]); m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
root=pathlib.Path(sys.argv[2]); root.mkdir(mode=0o700); os.environ['BOSS_RECLAIM_GIT_DIR']=str(root); m.MAX_RECORDS=2
for n in range(2):
 entry=m.reserve(); begin={'schema':1,'phase':'fixture','operation':'status','pid':n+1,'start_ticks':1,'boot_id':m.boot_id(),'cgroup_ns':1,'at_ns':n+1,'accounting_scope':'observer own cgroup','accounting':m.accounting()}
 m.write_record(entry,'begin.json',begin); m.write_record(entry,'end.json',{'schema':1,'pid':n+1,'exit_code':0,'observed':True,'at_ns':n+2,'accounting':m.accounting()})
original=m.write_record
def failure(path,name,row):
 if name=='retention.json': raise OSError('fixture durable counter refusal')
 original(path,name,row)
m.write_record=failure
try: m.reserve()
except OSError: pass
else: raise AssertionError('injected counter refusal was ignored')
assert m.state()['state']=='unknown', m.state()
pending=m.read_record(root,'retirement.json')
assert m.valid_begin(pending['begin']) and m.valid_end(pending['end'],pending['begin'])
assert pending['dropped_complete_before']==0
print('interrupted retirement retains original evidence and refuses a pass')
"#]).arg(repo_root().join("infra/cluster/dev-reclaim-git.py")).arg(root.join("ledger")).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn owning_sampler_freezes_safe_phase_evidence_beside_the_original_ring() {
    let root = scratch_dir("reclaim-git-freeze");
    let ring = root.join("memory/ring");
    create_dir(&ring);
    write_file(&ring.join("123.tsv"), "original ring bytes\n");
    let bin = root.join("bin");
    create_dir(&bin);
    write_exec(&bin.join("git"), "#!/bin/sh\nexit 7\n");
    let ledger = root.join("ledger");
    let helper = repo_root().join("infra/cluster/dev-reclaim-git.sh");
    let observed = Command::new("bash")
        .arg(&helper)
        .args(["--run", "fixture", "--", "status"])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("BOSS_RECLAIM_GIT_DIR", &ledger)
        .output()
        .unwrap();
    assert_eq!(observed.status.code(), Some(7));
    let source =
        std::fs::read_to_string(repo_root().join("infra/cluster/dev-memory-watch.sh")).unwrap();
    let script = root.join("freeze.sh");
    write_file(
        &script,
        &format!(
            "set -euo pipefail\n{}\n{}\n{}\nfreeze_ring \"$FROZEN\"\n",
            function(&source, "git_phase_snapshot").unwrap_or_default(),
            function(&source, "all_but_newest")
                .unwrap_or_else(|| "all_but_newest() { cat >/dev/null; }".to_owned()),
            function(&source, "freeze_ring").unwrap()
        ),
    );
    let frozen = root.join("memory/kills/fixture");
    let output = Command::new("bash")
        .arg(script)
        .env("DIR", root.join("memory"))
        .env(
            "SELF",
            repo_root().join("infra/cluster/dev-memory-watch.sh"),
        )
        .env("KILLS_KEEP", "8")
        .env("FROZEN", &frozen)
        .env("BOSS_RECLAIM_GIT_DIR", &ledger)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(frozen.join("123.tsv")).unwrap(),
        b"original ring bytes\n"
    );
    let bytes = std::fs::read(frozen.join("git-phases.json")).unwrap();
    let archive: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(archive["phases"][0]["terminal"]["exit_code"], 7);
    assert_eq!(archive["omitted"], 0);
}

#[test]
fn terminal_identity_fields_are_exact_integers_and_untyped_records_are_conserved() {
    let root = scratch_dir("reclaim-terminal-types");
    let output = Command::new("python3").args(["-c", r#"
import importlib.util,os,pathlib,sys
spec=importlib.util.spec_from_file_location('observer',sys.argv[1]); m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
root=pathlib.Path(sys.argv[2])
begin={'schema':1,'phase':'fixture','operation':'status','pid':1,'start_ticks':1,'boot_id':m.boot_id(),'cgroup_ns':1,'at_ns':1,'accounting_scope':'observer own cgroup','accounting':m.accounting()}
end={'schema':1,'pid':1,'exit_code':7,'observed':True,'at_ns':2,'accounting':m.accounting()}
assert m.valid_begin(begin) and m.valid_end(end,begin)
for index,(field,value) in enumerate([('schema',True),('schema',1.0),('pid',True),('pid',1.0)]):
 ledger=root/str(index); os.environ['BOSS_RECLAIM_GIT_DIR']=str(ledger)
 entry=m.reserve(); m.write_record(entry,'begin.json',begin); m.write_record(entry,'end.json',end)
 assert m.state()['state']=='observed' and m.snapshot(1)['phases'][0]['terminal']['exit_code']==7
 changed={**end,field:value}; m.write_record(entry,'end.json',changed)
 original={name:(entry/name).read_bytes() for name in ['begin.json','end.json']}
 assert not m.valid_end(changed,begin),(field,type(value).__name__,'accepted as an integer')
 assert m.state()['state']=='unknown'
 phase=m.snapshot(1)['phases'][0]; assert phase['status']=='unknown_terminal' and 'terminal' not in phase
 m.MAX_RECORDS=1
 try: m.reserve()
 except ValueError: pass
 else: raise AssertionError('untyped terminal was retired as a validated record')
 assert all((entry/name).read_bytes()==data for name,data in original.items())
 assert m.state()['dropped_complete']==0
print('integer terminal passes; four untyped terminals remain UNKNOWN and conserved')
"#]).arg(repo_root().join("infra/cluster/dev-reclaim-git.py")).arg(root.join("ledgers")).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn retirement_refuses_a_private_replacement_and_keeps_its_archive() {
    let root = scratch_dir("reclaim-retirement-binding");
    let output = Command::new("python3").args(["-c", r#"
import importlib.util,os,pathlib,sys
spec=importlib.util.spec_from_file_location('observer',sys.argv[1]); m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
root=pathlib.Path(sys.argv[2]); os.environ['BOSS_RECLAIM_GIT_DIR']=str(root)
entry=m.reserve()
begin={'schema':1,'phase':'fixture','operation':'status','pid':1,'start_ticks':1,'boot_id':m.boot_id(),'cgroup_ns':1,'at_ns':1,'accounting_scope':'observer own cgroup','accounting':m.accounting()}
end={'schema':1,'pid':1,'exit_code':7,'observed':True,'at_ns':2,'accounting':m.accounting()}
m.write_record(entry,'begin.json',begin); m.write_record(entry,'end.json',end)
m.MAX_RECORDS=1
original=root.parent/'original'; replacement=root.parent/'replacement'; replacement.mkdir(mode=0o700); replacement.chmod(0o700)
sentinel={name:('unrelated '+name).encode() for name in ['begin.json','end.json']}
for name,data in sentinel.items(): (replacement/name).write_bytes(data); (replacement/name).chmod(0o600)
write=m.write_record
def substitute(path,name,row):
 write(path,name,row)
 if name=='retirement.json': entry.rename(original); replacement.rename(entry)
m.write_record=substitute
try: m.reserve()
except (OSError,ValueError): pass
else: raise AssertionError('private directory substitution was accepted')
assert all((entry/name).read_bytes()==data for name,data in sentinel.items()),'unrelated replacement bytes changed'
assert m.read_record(original,'begin.json')==begin and m.read_record(original,'end.json')==end
pending=m.read_record(root,'retirement.json')
assert pending['begin']==begin and pending['end']==end and pending['dropped_complete_before']==0
assert m.state()['state']=='unknown'
print('replacement and original conserved; durable retirement retained UNKNOWN')
"#]).arg(repo_root().join("infra/cluster/dev-reclaim-git.py")).arg(root.join("ledger")).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn retirement_conserves_records_changed_inside_the_same_directory() {
    let root = scratch_dir("reclaim-retirement-record-binding");
    let output = Command::new("python3").args(["-c", r#"
import importlib.util,os,pathlib,sys
spec=importlib.util.spec_from_file_location('observer',sys.argv[1]); m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
root=pathlib.Path(sys.argv[2]); os.environ['BOSS_RECLAIM_GIT_DIR']=str(root)
entry=m.reserve(); identity=entry.stat().st_ino
begin={'schema':1,'phase':'fixture','operation':'status','pid':1,'start_ticks':1,'boot_id':m.boot_id(),'cgroup_ns':1,'at_ns':1,'accounting_scope':'observer own cgroup','accounting':m.accounting()}
end={'schema':1,'pid':1,'exit_code':7,'observed':True,'at_ns':2,'accounting':m.accounting()}
m.write_record(entry,'begin.json',begin); m.write_record(entry,'end.json',end)
m.MAX_RECORDS=1; write=m.write_record
replacement={name:('"unrelated '+name+'"\n').encode() for name in ['begin.json','end.json']}
def substitute(path,name,row):
 write(path,name,row)
 if name=='retirement.json':
  for filename,data in replacement.items(): (entry/filename).write_bytes(data)
  assert entry.stat().st_ino==identity
m.write_record=substitute
try: m.reserve()
except (OSError,ValueError): pass
else: raise AssertionError('same-directory record substitution was accepted')
assert all((entry/name).read_bytes()==data for name,data in replacement.items())
pending=m.read_record(root,'retirement.json'); assert pending['begin']==begin and pending['end']==end
assert m.state()['state']=='unknown'
"#]).arg(repo_root().join("infra/cluster/dev-reclaim-git.py")).arg(root.join("ledger")).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_missing_observer_does_not_replace_the_original_git_status() {
    let root = scratch_dir("reclaim-git-missing-helper");
    let bin = root.join("bin");
    create_dir(&bin);
    write_exec(
        &bin.join("git"),
        "#!/bin/sh\nprintf 'original output\\n'\nexit 7\n",
    );
    let source =
        std::fs::read_to_string(repo_root().join("infra/cluster/dev-scratch-reclaim.sh")).unwrap();
    let script = root.join("wrapper.sh");
    write_file(
        &script,
        &format!("{}\ngit status\n", function(&source, "git").unwrap()),
    );
    let output = Command::new("bash")
        .arg(script)
        .env("RECLAIM_GIT_OBSERVER", root.join("absent-helper"))
        .env("BOSS_RECLAIM_GIT_DIR", root.join("ledger"))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"original output\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown observer unavailable"));
}

#[test]
fn a_substituted_ledger_lock_is_unknown_without_following_the_target() {
    let root = scratch_dir("reclaim-git-lock");
    let output = Command::new("python3").args(["-c", r#"
import importlib.util,os,pathlib,sys
spec=importlib.util.spec_from_file_location('observer',sys.argv[1]); m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
root=pathlib.Path(sys.argv[2]); root.mkdir(mode=0o700); os.environ['BOSS_RECLAIM_GIT_DIR']=str(root)
entry=m.reserve(); begin={'schema':1,'phase':'fixture','operation':'status','pid':1,'start_ticks':1,'boot_id':m.boot_id(),'cgroup_ns':1,'at_ns':1,'accounting_scope':'observer own cgroup','accounting':m.accounting()}
m.write_record(entry,'begin.json',begin); m.write_record(entry,'end.json',{'schema':1,'pid':1,'exit_code':0,'observed':True,'at_ns':2,'accounting':m.accounting()})
sentinel=root.parent/'sentinel'; sentinel.write_text('conserved'); (root/'.lock').unlink(); (root/'.lock').symlink_to(sentinel)
assert m.state()['state']=='unknown',m.state()
assert sentinel.read_text()=='conserved'
print('linked lock refused')
"#]).arg(repo_root().join("infra/cluster/dev-reclaim-git.py")).arg(root.join("ledger")).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn original_git_stderr_suppression_cannot_hide_a_telemetry_refusal() {
    let root = scratch_dir("reclaim-git-visible-refusal");
    let bin = root.join("bin");
    create_dir(&bin);
    write_exec(
        &bin.join("git"),
        "#!/bin/sh\nprintf 'original stderr\\n' >&2\nexit 7\n",
    );
    let target = root.join("sentinel");
    create_dir(&target);
    std::os::unix::fs::symlink(target, root.join("unsafe")).unwrap();
    let source =
        std::fs::read_to_string(repo_root().join("infra/cluster/dev-scratch-reclaim.sh")).unwrap();
    let script = root.join("wrapper.sh");
    write_file(
        &script,
        &format!(
            "exec 9>&2\n{}\ngit status 2>/dev/null\n",
            function(&source, "git").unwrap()
        ),
    );
    let output = Command::new("bash")
        .arg(script)
        .env(
            "RECLAIM_GIT_OBSERVER",
            repo_root().join("infra/cluster/dev-reclaim-git.sh"),
        )
        .env("BOSS_RECLAIM_GIT_DIR", root.join("unsafe"))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("unknown storage"), "{diagnostic}");
    assert!(
        !diagnostic.contains("original stderr"),
        "original redirection remains intact"
    );
}

#[test]
fn prior_valid_records_do_not_mask_a_new_unrecordable_attempt_on_the_owning_pass() {
    let root = scratch_dir("reclaim-git-pass-refusal");
    let bin = root.join("bin");
    create_dir(&bin);
    write_exec(&bin.join("git"), "#!/bin/sh\nexit 7\n");
    let helper = repo_root().join("infra/cluster/dev-reclaim-git.sh");
    let ledger = root.join("ledger");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    assert_eq!(
        Command::new("bash")
            .arg(&helper)
            .args(["--run", "fixture", "--", "status"])
            .env("PATH", &path)
            .env("BOSS_RECLAIM_GIT_DIR", &ledger)
            .status()
            .unwrap()
            .code(),
        Some(7)
    );
    let injected = root.join("observer.sh");
    write_file(
        &injected,
        "#!/bin/sh\nexec python3 \"${0%.sh}.py\" \"$@\"\n",
    );
    write_file(
        &root.join("observer.py"),
        &format!(
            r#"import importlib.util,json,sys
spec=importlib.util.spec_from_file_location('observer',{:?}); m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
if sys.argv[1:]==['--state']: print(json.dumps(m.state()))
else:
 def failure(): raise OSError('injected new storage failure')
 m.reserve=failure
 sys.exit(m.run(sys.argv[2],sys.argv[4:]))
"#,
            repo_root()
                .join("infra/cluster/dev-reclaim-git.py")
                .to_str()
                .unwrap()
        ),
    );
    let source =
        std::fs::read_to_string(repo_root().join("infra/cluster/dev-scratch-reclaim.sh")).unwrap();
    let script = root.join("pass.sh");
    write_file(
        &script,
        &format!(
            "exec 9>\"$GIT_PHASE_ATTEMPT_FILE\"\n{}\n{}\ngit status 2>/dev/null\nprintf 'status=%s\\n' \"$?\"\ngit_phase_state\n",
            function(&source, "git").unwrap(),
            function(&source, "git_phase_state").unwrap_or_else(|| {
                "git_phase_state() { bash \"$RECLAIM_GIT_OBSERVER\" --state; }".to_owned()
            })
        ),
    );
    let attempt = root.join("attempt");
    write_file(&attempt, "");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&attempt, std::fs::Permissions::from_mode(0o600)).unwrap();
    let output = Command::new("bash")
        .arg(script)
        .env("PATH", path)
        .env("RECLAIM_GIT_OBSERVER", injected)
        .env("BOSS_RECLAIM_GIT_DIR", ledger)
        .env("GIT_PHASE_ATTEMPT_FILE", &attempt)
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("status=7\n"), "{text}");
    let row: serde_json::Value = serde_json::from_str(text.split_once('\n').unwrap().1).unwrap();
    assert_eq!(
        row["state"], "unknown",
        "retained record validity cannot prove attempt coverage: {row}"
    );
    assert_eq!(row["attempt_coverage"], "unknown");
    assert_eq!(row["records"], 1);
    assert!(std::fs::metadata(attempt).unwrap().len() <= 128);
}
