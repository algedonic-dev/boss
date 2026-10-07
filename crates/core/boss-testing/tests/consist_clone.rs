use boss_testing::{repo_root, scratch_dir};
use std::process::Command;

#[test]
fn bootstrap_checks_both_fetched_refs_before_checking_out_candidate() {
    let script = r#"
import functools, http.server, pathlib, subprocess, sys, threading
source, root = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
def git(cwd, *args):
    return subprocess.check_output(['git', '-C', str(cwd), *args], stderr=subprocess.PIPE).decode().strip()
seed = root / 'seed'
seed.mkdir()
git(seed, 'init')
git(seed, 'config', 'user.name', 'Fixture')
git(seed, 'config', 'user.email', 'fixture@example.invalid')
(seed / 'baseline').write_text('base')
git(seed, 'add', '.')
git(seed, 'commit', '-m', 'base')
base = git(seed, 'rev-parse', 'HEAD')
(seed / 'candidate').write_text('candidate')
git(seed, 'add', '.')
git(seed, 'commit', '-m', 'candidate')
head = git(seed, 'rev-parse', 'HEAD')
remote = root / 'remote.git'
subprocess.run(['git', 'clone', '--bare', str(seed), str(remote)], check=True, capture_output=True)
git(remote, 'update-ref', 'refs/heads/main', base)
git(remote, 'update-ref', 'refs/heads/assembled', head)
git(remote, 'update-server-info')
class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args): pass
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=str(root)))
thread = threading.Thread(target=server.serve_forever)
thread.start()
try:
    for label, expected_head, expected_base, accepted in [('different', head, base, True), ('wrong-head', base, base, False), ('wrong-base', head, head, False)]:
        checkout, home = root / label / 'repo', root / label / 'home'
        body = source.read_text().replace('/work/repo', str(checkout)).replace('/tmp/clone-home', str(home))
        result = subprocess.run(['bash', '-c', body, 'consist-clone',
            'http://127.0.0.1:' + str(server.server_port) + '/remote.git',
            'refs/heads/assembled', expected_head, 'refs/heads/main', expected_base], capture_output=True, timeout=30)
        assert (result.returncode == 0) == accepted, (label, result.returncode, result.stdout, result.stderr)
        if accepted:
            assert git(checkout, 'rev-parse', 'HEAD') == head
            assert git(checkout, 'rev-parse', 'refs/remotes/origin/main') == base
            assert git(checkout, 'config', '--get-regexp', 'remote.origin.url').endswith('/remote.git')
            assert 'credential' not in (checkout / '.git/config').read_text()
        else:
            probe = subprocess.run(['git', '-C', str(checkout), 'rev-parse', '--verify', 'HEAD'], capture_output=True)
            assert probe.returncode != 0, 'refused binding checked out candidate'
finally:
    server.shutdown()
    thread.join()
    server.server_close()
"#;
    let result = Command::new("python3")
        .args(["-c", script])
        .arg(repo_root().join("crates/orchestrators/boss-cli/src/train/consist_clone.sh"))
        .arg(scratch_dir("consist-clone"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
