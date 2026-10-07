"""Fake-material causal controls. Never run against live credentials or endpoints."""
import os
import importlib.util
import pathlib
import shutil
import sys
import subprocess
import tempfile
import unittest
import socketserver
import threading

# Artifacts contain only generated fake-material controls and compiler output.
ARTIFACTS = pathlib.Path(tempfile.mkdtemp(prefix='boss-isolation-rust-artifacts-'))
print('Synthetic fixture artifact directory:', ARTIFACTS, flush=True)
RUSTC = '/usr/local/cargo/bin/rustc'
SAFE_PATH = '/usr/bin:/bin'

def can_chroot():
    try:
        status = pathlib.Path('/proc/self/status').read_text()
        effective = next(line.split()[1] for line in status.splitlines() if line.startswith('CapEff:'))
        return bool(int(effective, 16) & (1 << 18))
    except (OSError, StopIteration, ValueError):
        return False

def sysroot():
    result = run_logged([RUSTC, '--print', 'sysroot'], env={'PATH': SAFE_PATH, 'RUSTUP_HOME': '/usr/local/rustup'}, capture_output=True)
    if result.returncode:
        raise RuntimeError(result.stderr.decode())
    return pathlib.Path(result.stdout.decode().strip())

def run_logged(argv, **kwargs):
    result = subprocess.run(argv, **kwargs)
    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    index = len(list(ARTIFACTS.glob('process-*.exit')))
    stem = ARTIFACTS / ('process-' + str(index))
    stem.with_suffix('.stdout').write_bytes(result.stdout)
    stem.with_suffix('.stderr').write_bytes(result.stderr)
    stem.with_suffix('.exit').write_text(str(result.returncode) + '\n')
    return result

FIXTURES = pathlib.Path(__file__).parent / 'fixtures'

class RustFixtureTests(unittest.TestCase):
    @unittest.skipUnless(can_chroot(), 'NOT RUN: stamping-proxy jail needs CAP_SYS_CHROOT')
    def test_hidden_file_does_not_stop_vulnerable_stamping_proxy(self):
        with tempfile.TemporaryDirectory(prefix='boss-proxy-fake-') as directory:
            root = pathlib.Path(directory)
            material = root / 'fake-material'
            material.write_text('synthetic-fixture-material-only\n')
            receipt = root / 'proxy-effect'
            class Receiver(socketserver.BaseRequestHandler):
                def handle(self):
                    request = self.request.recv(4096)
                    if request == b'STAMP synthetic-fixture-effect\n':
                        # Proxy holds synthetic authority; caller sends no material.
                        if material.read_bytes() == b'synthetic-fixture-material-only\n':
                            receipt.write_text('accepted synthetic effect\n')
                            self.request.sendall(b'ACCEPTED\n')
            server = socketserver.TCPServer(('127.0.0.1', 0), Receiver)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                env = {'PATH': SAFE_PATH, 'HOME': str(root), 'RUSTUP_HOME': '/usr/local/rustup',
                       'CARGO_HOME': str(root / 'cargo'), 'FIXTURE_PROXY': '127.0.0.1:' + str(server.server_address[1]),
                       'FIXTURE_MATERIAL': str(material)}
                binary = root / 'proxy-attack'
                compile_result = run_logged([RUSTC, str(FIXTURES / 'rust-stamping-proxy.rs'), '-o', str(binary)], env=env, capture_output=True)
                self.assertEqual(compile_result.returncode, 0, compile_result.stderr.decode())
                spec = importlib.util.spec_from_file_location('proxy_fixture_helper', FIXTURES.parent / 'isolation-fixture.py')
                helper = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(helper)
                jail = root / 'jail'
                work = jail / 'work'
                work.mkdir(parents=True)
                helper.copy_tool(jail, binary)
                result = helper.candidate([str(binary)], env, work, jail)
                (ARTIFACTS / 'proxy.stdout').write_bytes(result.stdout)
                (ARTIFACTS / 'proxy.stderr').write_bytes(result.stderr)
                (ARTIFACTS / 'proxy.exit').write_text(str(result.returncode) + '\n')
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                self.assertEqual(receipt.read_text(), 'accepted synthetic effect\n')
                self.assertIn(b'READ denied', result.stdout)
            finally:
                server.shutdown()
                server.server_close()
                thread.join()

    def test_cargo_build_script_attack(self):
        with tempfile.TemporaryDirectory(prefix='boss-cargo-fake-') as directory:
            root = pathlib.Path(directory)
            secret = root / 'fake-material'
            secret.write_text('synthetic-fixture-material-only\n')
            receipt = root / 'effect'
            project = root / 'project'
            shutil.copytree(FIXTURES / 'cargo-build-attack', project)
            shutil.copyfile(FIXTURES / 'rust-read-use.rs', project / 'build.rs')
            env = {'PATH': '/usr/bin:/bin', 'HOME': str(root), 'CARGO_HOME': str(root / 'cargo-home'),
                   'RUSTC': str(sysroot() / 'bin' / 'rustc'),
                   'FIXTURE_MATERIAL': str(secret), 'FIXTURE_RECEIPT': str(receipt)}
            cargo = str(sysroot() / 'bin' / 'cargo')
            result = run_logged([cargo, 'build', '--offline', '--manifest-path', str(project / 'Cargo.toml')], env=env, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            self.assertEqual(receipt.read_text(), 'accepted synthetic effect\n')
            receipt.unlink()
            secret.unlink()
            denied = run_logged([cargo, 'clean', '--manifest-path', str(project / 'Cargo.toml')], env=env, capture_output=True)
            self.assertEqual(denied.returncode, 0, denied.stderr.decode())
            denied = run_logged([cargo, 'build', '--offline', '--manifest-path', str(project / 'Cargo.toml')], env=env, capture_output=True)
            self.assertNotEqual(denied.returncode, 0)
            self.assertFalse(receipt.exists())

    def test_native_attack_reads_and_uses_generated_material(self):
        with tempfile.TemporaryDirectory(prefix='boss-rust-fake-') as directory:
            root = pathlib.Path(directory)
            secret = root / 'fake-material'
            secret.write_text('synthetic-fixture-material-only\n')
            receipt = root / 'accepted-effect'
            tool = RUSTC
            self.assertIsNotNone(tool)
            # No inherited actor, proxy, credential, cargo or endpoint variables.
            env = {'PATH': SAFE_PATH, 'HOME': str(root),
                   'RUSTUP_HOME': '/usr/local/rustup', 'CARGO_HOME': str(root / 'cargo'),
                   'FIXTURE_MATERIAL': str(secret), 'FIXTURE_RECEIPT': str(receipt)}
            result = run_logged([tool, str(FIXTURES / 'rust-read-use.rs'), '-o', str(root / 'attack')], env=env, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            run = run_logged([str(root / 'attack')], env=env, capture_output=True)
            self.assertEqual(run.returncode, 0, run.stderr.decode())
            self.assertEqual(receipt.read_text(), 'accepted synthetic effect\n')
            receipt.unlink()
            # rustc loads and executes the proc macro in this fake-only harness.
            macro = root / 'libsynthetic_attack.so'
            compile_macro = run_logged([tool, '--crate-type', 'proc-macro', '--crate-name', 'synthetic_attack', str(FIXTURES / 'rust-proc-macro.rs'), '-o', str(macro)], env=env, capture_output=True)
            self.assertEqual(compile_macro.returncode, 0, compile_macro.stderr.decode())
            consumer = run_logged([tool, str(FIXTURES / 'rust-proc-macro-consumer.rs'), '--extern', 'synthetic_attack=' + str(macro), '-o', str(root / 'consumer')], env=env, capture_output=True)
            self.assertEqual(consumer.returncode, 0, consumer.stderr.decode())
            self.assertEqual(receipt.read_text(), 'accepted synthetic effect\n')
            receipt.unlink()
            test_compile = run_logged([tool, '--test', str(FIXTURES / 'rust-read-use.rs'), '-o', str(root / 'tests')], env=env, capture_output=True)
            self.assertEqual(test_compile.returncode, 0, test_compile.stderr.decode())
            test_run = run_logged([str(root / 'tests')], env=env, capture_output=True)
            self.assertEqual(test_run.returncode, 0, test_run.stderr.decode())
            self.assertTrue(receipt.exists())
            receipt.unlink()
            if can_chroot():
                spec = importlib.util.spec_from_file_location('fixture_helper', FIXTURES.parent / 'isolation-fixture.py')
                helper = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(helper)
                jail = root / 'jail'
                work = jail / 'work'
                work.mkdir(parents=True)
                helper.copy_tool(jail, root / 'attack')
                jailed_binary = '/' + str((root / 'attack')).lstrip('/')
                isolated = helper.candidate([jailed_binary], env, work, jail)
                (ARTIFACTS / 'jail.stdout').write_bytes(isolated.stdout)
                (ARTIFACTS / 'jail.stderr').write_bytes(isolated.stderr)
                (ARTIFACTS / 'jail.exit').write_text(str(isolated.returncode) + '\n')
                self.assertNotEqual(isolated.returncode, 0)
                self.assertFalse(receipt.exists())
            else:
                print('NOT RUN: native candidate jail needs CAP_SYS_CHROOT', flush=True)
            secret.unlink()
            denied = run_logged([str(root / 'attack')], env=env, capture_output=True)
            self.assertNotEqual(denied.returncode, 0)
            self.assertFalse(receipt.exists())

if __name__ == '__main__':
    unittest.main()
