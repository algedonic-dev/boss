#!/usr/bin/env python3
"""67e08c6a: fixture credentials are synthetic, never live export proof."""
import importlib.util
import ast
import contextlib
import io
import json
import os
from pathlib import Path
import stat
import shutil
import signal
import time
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

SCRIPT = Path(__file__).with_name('proton_bridge.py')
ACCOUNT = 'agent-b@algedonic.dev'
SECRET = 'SYNTHETIC-FIXTURE-NOT-A-CREDENTIAL'  # dummy parser fixture, not a credential


def module():
    spec = importlib.util.spec_from_file_location('proton_bridge', SCRIPT)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def process_state(pid):
    """The /proc state letter of `pid`, or None once it has been reaped.

    09775eb3 (main red at 7871c94d, gate-run b8abf221): the timeout test asked
    `state.exists()` and then read the file, and the descendant was reaped
    between the two, so the read raised FileNotFoundError and the suite ERRORED
    on a process that was gone, which is the outcome it asserts. One read,
    and absence answers reaped."""
    try:
        text = (Path('/proc') / str(pid) / 'stat').read_text()
    except (FileNotFoundError, ProcessLookupError):
        return None
    # The comm field is parenthesised and may hold spaces; the state follows it.
    return text.rsplit(')', 1)[1].split()[0]


def recorded_pid(path, seconds=1.0):
    """The pid a fixture descendant wrote to `path`, or None if it never did.

    09775eb3: under load the descendant can be SIGKILLed with its session before
    it writes its pid (or between the open and the write), and an unguarded
    read then raised instead of judging the session. Bounded, never a hang."""
    deadline = time.monotonic() + seconds
    while True:
        with contextlib.suppress(FileNotFoundError):
            text = path.read_text().strip()
            if text:
                return int(text)
        if time.monotonic() >= deadline:
            return None
        time.sleep(0.01)


def unprivileged_capture():
    if os.geteuid() == 0:
        raise unittest.SkipTest('a real capture entrypoint must never run as root in a fixture')
    return subprocess.run([sys.executable, str(SCRIPT), 'capture'], capture_output=True)


def configuration(secret=SECRET):
    return (f'Configuration for {ACCOUNT}\nIMAP Settings\nAddress:   127.0.0.1\n'
            f'IMAP port: 1143\nUsername:  {ACCOUNT}\nPassword:  {secret}\nSecurity:  STARTTLS\n'
            f'SMTP Settings\nAddress:   127.0.0.1\nSMTP port: 1025\nUsername:  {ACCOUNT}\n'
            f'Password:  {secret}\nSecurity:  STARTTLS\n')


# 34d95817: the pinned CLI's `list` answer (internal/frontend/cli/accounts.go at
# 43b92377): a bold header whose reset code lands on the next line, one
# '%-2d: %-20s (%-15s, %-15s)' row per vault account, then one empty line.
LIST_HEADER = '\x1b[1m# : account              (status         , address mode   )\n\x1b[0m'


def listing(*states, name='Agent B'):
    rows = ''.join(f'{index:<2d}: {name:<20s} ({state:<15s}, {"combined":<15s})\n'
                   for index, state in enumerate(states))
    return LIST_HEADER + rows + '\n'


def fake_bridge(directory, loaded_after):
    """A synthetic, non-secret stand-in for `proton-bridge --cli` at the pin.

    The vault account reads Locked until its asynchronous load finishes
    (bridge.GetUserInfo answers Locked for a vault user not yet in
    bridge.users; loadUsers runs as a task started beside the shell), and
    `info` then prints no configuration block. That is the window the fixed
    'info ADDRESS / exit' input of dbe1bf9a was answered in."""
    child = Path(directory) / 'fake-bridge.py'
    child.write_text(
        'import sys, time\n'
        'start = time.monotonic()\n'
        f'LOADED_AFTER = {loaded_after!r}\n'
        f'HEADER = {LIST_HEADER!r}\n'
        f'CONFIG = {configuration()!r}\n'
        'def state():\n'
        "    return 'connected' if time.monotonic() - start >= LOADED_AFTER else 'locked'\n"
        "sys.stdout.write('Welcome to Proton Mail Bridge interactive shell\\n'); sys.stdout.flush()\n"
        'for line in sys.stdin:\n'
        '    words = line.split()\n'
        "    if words == ['exit']:\n"
        '        break\n'
        "    if words == ['list']:\n"
        "        sys.stdout.write(HEADER + '%-2d: %-20s (%-15s, %-15s)\\n' % (0, 'Agent B', state(), 'combined') + '\\n')\n"
        "    elif words[:1] == ['info']:\n"
        "        if state() == 'locked':\n"
        "            sys.stdout.write('User \\x1b[1mAgent B\\x1b[0m is currently locked. Please wait and try again.\\n')\n"
        '        else:\n'
        '            sys.stdout.write(CONFIG)\n'
        '    sys.stdout.flush()\n')
    return [sys.executable, str(child)]


class BridgeAccountSelection(unittest.TestCase):
    """34d95817: capture refused 'no connected account' while Bridge listed the
    one account as connected. The pinned CLI selects the sole account whatever
    `info`'s argument is, so the address was never the fault: the fixed input
    asked before the account had finished loading. The capture now reads the
    account list, waits while it reads locked, and asks `info` by index."""

    def test_the_fixed_input_is_answered_before_the_account_loads(self):
        # The reproduction, in the shape dbe1bf9a ran: everything on stdin at once.
        m = module()
        with tempfile.TemporaryDirectory() as directory:
            raw = m.private_cli(fake_bridge(directory, 0.6), dict(os.environ), 5,
                                data=b'info agent-b@algedonic.dev\nexit\n')
        with self.assertRaises(m.Refused) as refused:
            m.parse_configuration(raw, ACCOUNT)
        self.assertEqual(refused.exception.args,
                         ('no connected account configuration; the owner login is missing or signed out',))

    def test_capture_waits_for_the_loading_account_and_asks_by_index(self):
        m = module(); seen = []
        conversation = m.capture_conversation()
        def respond(raw, now):
            seen.append(raw)
            return conversation(raw, now)
        with tempfile.TemporaryDirectory() as directory, patch.object(m, 'LIST_POLL_SECONDS', 0.2):
            raw = m.private_cli(fake_bridge(directory, 0.6), dict(os.environ), 5, respond=respond)
        row = m.parse_configuration(raw, ACCOUNT)
        self.assertEqual(row['password'], SECRET)
        self.assertEqual(row['account'], ACCOUNT)
        # The conversation decides from the account list alone: it is never
        # handed output that could carry the configuration's password.
        self.assertTrue(seen)
        self.assertFalse(any(SECRET.encode() in raw for raw in seen))

    def test_an_account_that_never_loads_refuses_with_its_own_reason(self):
        m = module()
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(m, 'LIST_POLL_SECONDS', 0.1), patch.object(m, 'ACCOUNT_WAIT_SECONDS', 0.5):
            with self.assertRaises(m.Refused) as refused:
                m.private_cli(fake_bridge(directory, 1000), dict(os.environ), 5,
                              respond=m.capture_conversation())
        self.assertEqual(refused.exception.args,
                         ('the Bridge account was still loading or locked at the wait bound; credential unchanged',))

    def test_a_cli_that_exits_before_selection_refuses(self):
        m = module()
        with tempfile.TemporaryDirectory() as directory:
            child = Path(directory) / 'gone.py'; child.write_text('print("bye")\n')
            with self.assertRaises(m.Refused) as refused:
                m.private_cli([sys.executable, str(child)], dict(os.environ), 3,
                              respond=m.capture_conversation())
        self.assertIn(refused.exception.args[0], {
            'CLI exited before an account was selected; credential unchanged',
            'CLI failed; credential unchanged'})

    def test_account_rows_read_the_last_complete_list_answer(self):
        m = module()
        banner = b'\x1b[33mWARN\x1b[0m[Nov 20 12:00:00.000] startup\nWelcome to Proton Mail Bridge interactive shell\n'
        self.assertIsNone(m.account_rows(b''))
        self.assertIsNone(m.account_rows(banner))
        self.assertEqual(m.account_rows(banner + listing().encode()), [])
        self.assertEqual(m.account_rows(listing('locked').encode()), ['locked'])
        # The newest answer wins over an older one.
        self.assertEqual(m.account_rows((listing('locked') + listing('connected')).encode()), ['connected'])
        self.assertEqual(m.account_rows(listing('signed out', 'connected').encode()), ['signed out', 'connected'])
        # A long username is not padded, and is never what the state is read from.
        self.assertEqual(m.account_rows(listing('connected', name='x (locked , split )' * 3).encode()), ['connected'])
        # An answer still arriving, or one an asynchronous line split, reads as none yet.
        self.assertIsNone(m.account_rows(listing('connected').encode()[:-1]))
        self.assertIsNone(m.account_rows(listing('connected').encode()[:-2]))
        split = listing('signed out', 'connected').replace('\n1 ', '\nA sync has begun for Agent B.\n1 ')
        self.assertIsNone(m.account_rows(split.encode()))
        self.assertIsNone(m.account_rows(listing('connected').replace('0 :', '1 :').encode()))
        self.assertIsNone(m.account_rows(listing('connected').replace('connected', 'unknowable').encode()))

    def test_conversation_lists_polls_then_selects_by_index(self):
        m = module(); say = m.capture_conversation()
        self.assertEqual(say(b'', 0.0), (b'list\n', False))
        self.assertEqual(say(b'', 0.5), (None, False))
        locked = listing('locked').encode()
        self.assertEqual(say(locked, 0.9), (None, False))
        self.assertEqual(say(locked, 1.0), (b'list\n', False))
        self.assertEqual(say(locked + listing('signed out', 'connected').encode(), 1.2), (b'info 1\nexit\n', True))
        self.assertEqual(m.capture_conversation()(listing('connected').encode(), 0.0), (b'info 0\nexit\n', True))

    def test_conversation_refuses_each_settled_state_with_its_own_reason(self):
        m = module()
        cases = (
            (listing(), 'no Bridge account is configured; the owner login is missing'),
            (listing('signed out'), 'no connected Bridge account; the owner login is signed out'),
            (listing('signed out', 'signed out'), 'no connected Bridge account; the owner login is signed out'),
            (listing('connected', 'connected'),
             'more than one connected account configuration; a second account must be signed out'),
        )
        for raw, reason in cases:
            with self.subTest(reason=reason):
                with self.assertRaises(m.Refused) as refused:
                    m.capture_conversation()(raw.encode(), 0.0)
                self.assertEqual(refused.exception.args, (reason,))
                self.assertIn(reason, m.SAFE_REFUSALS)
                self.assertNotIn('Agent B', reason)
        # A row still loading may yet connect: wait rather than judge the others.
        self.assertEqual(m.capture_conversation()(listing('connected', 'locked').encode(), 0.0), (b'list\n', False))

    def test_conversation_wait_bound_names_what_it_was_waiting_for(self):
        m = module()
        for raw, reason in (
            (b'', 'Bridge printed no readable account list before the wait bound; credential unchanged'),
            (listing('locked').encode(), 'the Bridge account was still loading or locked at the wait bound; credential unchanged'),
        ):
            with self.subTest(reason=reason):
                say = m.capture_conversation()
                self.assertEqual(say(raw, 0.0)[1], False)
                with self.assertRaises(m.Refused) as refused:
                    say(raw, float(m.ACCOUNT_WAIT_SECONDS))
                self.assertEqual(refused.exception.args, (reason,))
        self.assertLess(m.ACCOUNT_WAIT_SECONDS, m.CLI_SECONDS, 'info and exit still fit inside the CLI deadline')

    def test_capture_entrypoint_converses_instead_of_fixed_input(self):
        m = module(); calls = []
        def private_cli(argv, env, **kwargs):
            calls.append(kwargs)
            return configuration().encode()
        def service(action):
            return 'inactive/dead/0' if action == 'state' else ''
        with patch.object(m.os, 'geteuid', return_value=0), patch.object(m, 'singleton', return_value=contextlib.nullcontext()), \
                patch.object(m, 'service', side_effect=service), patch.object(m, 'private_cli', side_effect=private_cli), \
                patch.object(m, 'write_credential'), patch.object(m, 'PRIVATE', Path('/nonexistent-fixture')), \
                patch.object(Path, 'stat', return_value=SimpleNamespace(st_size=1)), \
                patch.object(sys, 'argv', [str(SCRIPT), 'capture']), contextlib.redirect_stdout(io.StringIO()):
            m.main()
        self.assertEqual(len(calls), 1)
        self.assertNotIn('data', calls[0])
        self.assertTrue(callable(calls[0].get('respond')))
        self.assertFalse(hasattr(m, 'INPUT'), 'the fixed info/exit input is retired')


class BridgeContract(unittest.TestCase):
    def test_refusal_allowlist_equals_authored_literal_raises(self):
        m = module()
        calls = [node for node in ast.walk(ast.parse(SCRIPT.read_text()))
                 if isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
                 and node.func.id == 'Refused']
        self.assertTrue(calls)
        self.assertTrue(all(len(node.args) == 1 and isinstance(node.args[0], ast.Constant)
                            and isinstance(node.args[0].value, str) for node in calls),
                        'A refusal reason must never interpolate CLI or exception values')
        self.assertEqual(m.SAFE_REFUSALS, {node.args[0].value for node in calls})

    def test_actual_cli_sentinel_is_private_even_when_parser_or_child_refuses(self):
        m = module()
        guard = ast.Module(body=[ast.parse(SCRIPT.read_text()).body[-1]], type_ignores=[])
        with tempfile.TemporaryDirectory() as directory:
            child = Path(directory) / 'cli.py'
            for stream, code in (('stdout', 0), ('stderr', 0), ('stdout', 1)):
                child.write_text('import sys\nsys.stdin.buffer.read()\nsys.' + stream
                                 + '.write(' + repr(configuration() + 'Password: ' + SECRET)
                                 + ')\nsys.exit(' + str(code) + ')\n')
                def capture():
                    raw = m.private_cli([sys.executable, str(child)], dict(os.environ), 3)
                    m.parse_configuration(raw, ACCOUNT)
                stdout, stderr = io.StringIO(), io.StringIO()
                with patch.object(m, 'main', side_effect=capture), contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                    with self.assertRaises(SystemExit) as ended:
                        exec(compile(guard, str(SCRIPT), 'exec'), dict(vars(m), __name__='__main__'))
                self.assertEqual(ended.exception.code, 75)
                self.assertEqual(stdout.getvalue(), '')
                self.assertNotIn(SECRET, stderr.getvalue())
                self.assertNotIn('Password:', stderr.getvalue())
                self.assertIn('Refused: ', stderr.getvalue())

    def test_entrypoint_names_only_authored_refusals_and_never_cli_values(self):
        m = module()
        # Execute the source's actual __main__ guard with only the operation
        # port replaced; no live host, credential, service or Bridge process.
        guard = ast.Module(body=[ast.parse(SCRIPT.read_text()).body[-1]], type_ignores=[])
        for failure, reason in (
            (lambda: m.parse_configuration(configuration().encode() + b'Password: ' + SECRET.encode(), ACCOUNT),
             'configuration contains ambiguous fields'),
            (lambda: (_ for _ in ()).throw(m.Refused(SECRET)), None),
            (lambda: (_ for _ in ()).throw(ValueError(SECRET)), None),
            (lambda: (_ for _ in ()).throw(OSError(SECRET)), None),
            (lambda: (_ for _ in ()).throw(KeyError(SECRET)), None),
            (lambda: (_ for _ in ()).throw(subprocess.SubprocessError(SECRET)), None),
        ):
            stdout, stderr = io.StringIO(), io.StringIO()
            with patch.object(m, 'main', side_effect=failure), contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                with self.assertRaises(SystemExit) as ended:
                    exec(compile(guard, str(SCRIPT), 'exec'), dict(vars(m), __name__='__main__'))
            self.assertEqual(ended.exception.code, 75)
            self.assertEqual(stdout.getvalue(), '')
            self.assertNotIn(SECRET, stderr.getvalue())
            self.assertIn('UNAVAILABLE', stderr.getvalue())
            self.assertIn('outcome unconfirmed', stderr.getvalue())
            if reason:
                self.assertIn('Refused: ' + reason, stderr.getvalue())
            else:
                self.assertEqual(stderr.getvalue(), 'Bridge operation UNAVAILABLE; credential file outcome unconfirmed\n')

    def test_bridge_refusal_does_not_short_circuit_real_converge(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); source = root / ('a' * 40); infra = source / 'infra'
            # Adoption/authentication are boundary ports, not this test's claim.
            # The converge, unit installer, host checks, cap and stamp stay real.
            shutil.copytree(SCRIPT.parent.parent, infra)
            (source / '.git').mkdir()
            prep = root / 'prepared'; boot = prep / 'bootstrap/infra/gcp'; boot.mkdir(parents=True)
            units = prep / 'units'; units.mkdir()
            (units / 'accepted-sha').write_text(source.name + '\n')
            for path in (infra / 'gcp/root-tree-units').glob('*.service.in'):
                shutil.copyfile(path, units / path.name.removesuffix('.in'))
            def executable(path, body):
                path.write_text('#!/bin/bash\nset -eu\n' + body); path.chmod(0o755)
            executable(boot / 'root-tree.sh', 'printf "%s\\n" "$FIXTURE_ACCEPTED"\n')
            executable(boot / 'prepare-root-tree.sh', 'exit 0\n')
            executable(infra / 'gcp/adopt-root-tree.sh', 'exit 0\n')
            executable(infra / 'gcp/proton-bridge.sh', 'echo "fixture Bridge refusal: session already owned" >&2\nexit 75\n')
            for name in ('gcp/install-runner-credential-receiver.sh', 'gcp/install-root-tree-token-receiver.sh', 'ops/install-ops-runner.sh', 'journal-door-ensure.sh', 'estate/machine-token-effect.sh'):
                executable(infra / name, 'exit 0\n')
            executable(infra / 'gcp/prepared-deposit-keys.sh', 'echo "deposit-key-audit: UNVERIFIED fixture"\nexit 2\n')
            executable(infra / 'gcp/audit-deposit-keys.sh', 'echo "deposit-key-audit: clean"\n')
            (infra / 'estate/node-roles.sh').write_text('has_role() { [ "$1" = ops-runner ]; }\nread_node_roles() { export BOSS_NODE_ROLES=ops-runner BOSS_NODE_ROLES_SOURCE=registry; }\n')
            (infra / 'estate/install-cluster-operator.sh').write_text('install_cluster_operator() { return 99; }\n')
            (infra / 'estate/roles.toml').write_text('[always]\nunits=["boss-gcp-converge"]\n[roles.ops-runner]\nunits=[]\n')
            systemctl = root / 'systemctl'; executable(systemctl, 'printf "%s\\n" "$*" >> "$FIXTURE_CALLS"\n')
            cli = root / 'cli'; executable(cli, 'echo "fixture CLI reached"\n')
            etc = root / 'units'; etc.mkdir()
            env = dict(os.environ, FIXTURE_ACCEPTED=str(source), FIXTURE_CALLS=str(root / 'calls'),
                       BOSS_GCP_PREPARED_DIR=str(prep), BOSS_GCP_ROOT_TREE=str(root / 'tree'), BOSS_GCP_DEPOSIT_USER='fixture',
                       BOSS_GCP_REPO_DIR=str(source), BOSS_GCP_CONVERGED_FILE=str(root / 'converged'),
                       BOSS_GCP_CONVERGE_SOR_ENV=str(root / 'sor.env'), BOSS_GCP_CONVERGE_CLI_INSTALLER=str(cli),
                       BOSS_GCP_CONVERGE_AUTHORIZED_KEYS=str(root / 'absent-keys'), BOSS_RUN_SUMMARY_FILE=str(root / 'summary.json'),
                       BOSS_JOURNALD_CONF_DIR=str(root / 'journal'), BOSS_JOURNALD_SYSTEMCTL=str(systemctl),
                       INSTALL_ETC=str(etc), INSTALL_SYSTEMCTL=str(systemctl), INSTALL_APT_GET=str(systemctl),
                       BOSS_OPS_RUNNER_RETIRED=str(root / 'retired'), INSTALL_TALOSCTL='0', BOSS_JOBS_URL='http://fixture.invalid')
            result = subprocess.run(['/bin/bash', str(infra / 'gcp/boss-gcp-converge.sh'), '--root-generation'], env=env, capture_output=True, timeout=20)
            output = result.stdout + result.stderr
            self.assertEqual(result.returncode, 0, output.decode())
            self.assertIn(b'fixture Bridge refusal: session already owned', output)
            self.assertIn(b'Bridge install UNAVAILABLE (exit 75)', output)
            self.assertIn(b'fixture CLI reached', output)
            self.assertEqual((root / 'converged').read_text().strip(), source.name)
            self.assertIn('SystemMaxUse=256M', (root / 'journal/boss-gcp-journal-cap.conf').read_text())

    def test_entrypoint_refuses_an_off_host_before_the_child(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'gcp').mkdir(); (root / 'lib').mkdir(); (root / 'estate').mkdir()
            (root / 'gcp/proton-bridge.sh').write_bytes(SCRIPT.with_name('proton-bridge.sh').read_bytes())
            (root / 'lib/host-check.sh').write_bytes(SCRIPT.parent.parent.joinpath('lib/host-check.sh').read_bytes())
            # A documentation-only address cannot be this worker's identity.
            (root / 'estate/estate.toml').write_text('[[node]]\nid="boss-gcp"\naddress="192.0.2.254"\n')
            (root / 'gcp/proton_bridge.py').write_text('print("CHILD MUST NOT RUN")\n')
            env = dict(os.environ); env.pop('BOSS_FIRST_INSTALL_AS', None); env.pop('BOSS_HOST_CHECK_ESTATE', None)
            result = subprocess.run(['/bin/bash', str(root / 'gcp/proton-bridge.sh'), 'capture'], env=env, capture_output=True)
            self.assertEqual(result.returncode, 78, result.stdout + result.stderr)
            self.assertNotIn(b'CHILD MUST NOT RUN', result.stdout)
            self.assertIn(b'nothing was written', result.stderr)

    def test_failed_stop_still_attempts_restart_without_opening_cli(self):
        m = module(); calls = []
        def service(action):
            calls.append(action)
            if action == 'stop':
                raise m.Refused('stop acknowledgement unavailable')
        with self.assertRaises(m.Refused):
            m.coordinated_capture(service, lambda: self.fail('CLI must not run'), ACCOUNT)
        self.assertEqual(calls, ['stop', 'start'])

    def test_capture_service_reads_and_writes_have_one_declared_bounded_budget(self):
        m = module(); limits = []
        def checked(argv, **kwargs):
            limits.append(kwargs.get('seconds', 45))
            return b'ActiveState=inactive\nSubState=dead\nMainPID=0\n' if 'show' in argv else b''
        with patch.object(m, 'checked', side_effect=checked):
            m.service('stop'); m.service('state'); m.service('start')
        spec = json.loads(SCRIPT.parent.parent.joinpath('ops/verbs/capture-bridge-credential.json').read_text())
        total = sum(limits) + m.CLI_SECONDS + m.CLI_CLEANUP_SECONDS
        self.assertLess(total, spec['timeout'], 'state, stop, CLI, restart and cleanup all fit before runner deadline')
        self.assertIn(f'{m.SERVICE_CHANGE_SECONDS}s stop +{m.SERVICE_STATE_SECONDS}s state +{m.CLI_SECONDS}s CLI +{m.SERVICE_CHANGE_SECONDS}s restart +{m.CLI_CLEANUP_SECONDS}s CLI cleanup ={total}s', spec['about'])

    def test_singleton_parent_is_private_and_linked_or_public_parent_refuses(self):
        m = module()
        self.assertEqual(m.LOCK_DIR, Path('/run/boss-bridge'))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); private = root / 'private'
            with patch.object(m, 'LOCK_DIR', private):
                with m.singleton():
                    self.assertEqual(stat.S_IMODE(private.stat().st_mode), 0o700)
                    self.assertEqual(private.stat().st_uid, os.geteuid())
                private.chmod(0o777)
                with self.assertRaises(m.Refused):
                    with m.singleton(): self.fail('public lock parent')
            link = root / 'linked'; link.symlink_to(private, target_is_directory=True)
            with patch.object(m, 'LOCK_DIR', link), self.assertRaises(m.Refused):
                with m.singleton(): self.fail('linked lock parent')

    def test_singleton_never_opens_a_lock_in_a_shared_parent(self):
        m = module(); opened = []
        def refuse_open(path, *args, **kwargs):
            opened.append(Path(path)); raise m.Refused('fixture stops before opening')
        parent = SimpleNamespace(st_mode=stat.S_IFDIR | 0o700, st_uid=os.geteuid())
        with patch.object(Path, 'mkdir'), patch.object(Path, 'lstat', return_value=parent), patch.object(m.os, 'open', side_effect=refuse_open), self.assertRaises(m.Refused):
            with m.singleton(): self.fail('fixture must not enter')
        self.assertTrue(opened)
        self.assertEqual(opened[0].parent, Path('/run/boss-bridge'))

    def test_a_process_reaped_between_lookup_and_read_answers_reaped(self):
        # 09775eb3: the race that errored main's full gate at 7871c94d, planted
        # directly — the stat file vanishes (or the kernel answers ESRCH) at the
        # moment it is read. Reaped is the answer, never an exception.
        for gone in (FileNotFoundError, ProcessLookupError):
            with patch.object(Path, 'read_text', side_effect=gone):
                self.assertIsNone(process_state(os.getpid()))
        self.assertIn(process_state(os.getpid()), ('R', 'S'))

    def test_a_pid_file_never_written_answers_none_without_raising(self):
        # 09775eb3: the session can be killed before the descendant records
        # itself, or between the file's open and its write.
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'child-pid'
            self.assertIsNone(recorded_pid(path, seconds=0))
            path.write_text('')
            self.assertIsNone(recorded_pid(path, seconds=0))
            path.write_text('4242\n')
            self.assertEqual(recorded_pid(path, seconds=0), 4242)

    def test_cli_timeout_reaps_the_session_even_when_its_leader_exited(self):
        m = module()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); pid = root / 'child-pid'; child = root / 'child.py'
            child.write_text('import os,sys,time\nfrom pathlib import Path\nif os.fork()==0:\n Path(sys.argv[1]).write_text(str(os.getpid()))\n time.sleep(30)\n')
            try:
                with self.assertRaises(m.Refused):
                    m.private_cli([sys.executable, str(child), str(pid)], dict(os.environ), 0.3)
                # None: the session was killed before the descendant recorded
                # itself, so there is no live descendant to find (09775eb3).
                child_pid = recorded_pid(pid)
                deadline = time.monotonic() + 1
                while child_pid is not None and process_state(child_pid) not in (None, 'Z') and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertIn(None if child_pid is None else process_state(child_pid), (None, 'Z'), 'timed-out CLI session descendant is still running')
            finally:
                leftover = recorded_pid(pid, seconds=0)
                if leftover is not None:
                    with contextlib.suppress(ProcessLookupError): os.kill(leftover, signal.SIGKILL)

    def test_real_capture_test_port_refuses_root_before_subprocess(self):
        with patch.object(os, 'geteuid', return_value=0), patch.object(subprocess, 'run', side_effect=AssertionError('root capture must never execute')):
            with self.assertRaises(unittest.SkipTest):
                unprivileged_capture()

    def test_capture_child_reports_only_fixed_machine_fields_after_durable_write(self):
        m = module()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = io.StringIO()
            row = m.parse_configuration(configuration().encode(), ACCOUNT)
            real_write = m.write_credential
            uid = os.geteuid()
            def write_as_worker(path, value):
                with patch.object(m.os, 'geteuid', return_value=uid):
                    real_write(path, value)
            with patch.object(m.os, 'geteuid', return_value=0), patch.object(m, 'write_credential', side_effect=write_as_worker), patch.object(m, 'singleton', return_value=contextlib.nullcontext()), patch.object(m, 'PRIVATE', root), patch.object(m, 'coordinated_capture', return_value=row), patch.object(sys, 'argv', [str(SCRIPT), 'capture']), contextlib.redirect_stdout(output):
                m.main()
            target = root / 'adapter-credential.json'
            self.assertEqual(output.getvalue(), f'path={target} bytes={target.stat().st_size}\n')
            self.assertNotIn(SECRET, output.getvalue())

    def test_shell_judges_child_report_before_printing_effect(self):
        wrapper = SCRIPT.with_name('proton-bridge.sh').read_bytes()
        good = 'path=/etc/boss-bridge/adapter-credential.json bytes=123'
        cases = [(good, 0, True), (good, 75, False), ('', 0, False),
                 (good.replace('123', '0'), 0, False),
                 (good.replace('/etc/boss-bridge', '/tmp'), 0, False),
                 (good + '\nextra=' + SECRET, 0, False),
                 (good + ' trailing', 0, False),
                 (good.replace('123', '-1'), 0, False)]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); (root / 'gcp').mkdir(); (root / 'lib').mkdir()
            entry = root / 'gcp/proton-bridge.sh'; entry.write_bytes(wrapper)
            (root / 'lib/host-check.sh').write_text('host_path() { :; }\nhost_command() { :; }\nhost_check() { return 0; }\n')
            child = root / 'gcp/proton_bridge.py'
            for report, status, passes in cases:
                with self.subTest(report=report, status=status):
                    child.write_text('import sys\nsys.stdout.write(' + repr(report + '\n') + ')\nsys.exit(' + str(status) + ')\n')
                    result = subprocess.run(['/bin/bash', str(entry), 'capture'], capture_output=True)
                    if passes:
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertEqual(result.stdout, b'captured local Bridge credential: path=/etc/boss-bridge/adapter-credential.json bytes=123; real endpoint verification still owed\n')
                    else:
                        self.assertNotEqual(result.returncode, 0)
                        self.assertNotIn(b'captured local Bridge credential:', result.stdout)
                    self.assertNotIn(SECRET.encode(), result.stdout + result.stderr)

    def test_shell_does_not_repeat_a_refusal_the_child_already_named(self):
        # Review 7301d10f: the wrapper echoed a second generic UNAVAILABLE line
        # under the child's own named refusal, so every refusal read twice. The
        # child's line is forwarded once; the generic line is for a child that
        # died without a word, whatever its exit code.
        wrapper = SCRIPT.with_name('proton-bridge.sh').read_bytes()
        named = ('Bridge operation UNAVAILABLE; Refused: no connected account configuration; '
                 'the owner login is missing or signed out; credential file outcome unconfirmed\n')
        generic = 'Bridge operation UNAVAILABLE; credential file outcome unconfirmed\n'
        cases = [(named, 75, named), ('', 75, generic), ('', 1, generic), (named, 1, named)]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); (root / 'gcp').mkdir(); (root / 'lib').mkdir()
            entry = root / 'gcp/proton-bridge.sh'; entry.write_bytes(wrapper)
            (root / 'lib/host-check.sh').write_text('host_path() { :; }\nhost_command() { :; }\nhost_check() { return 0; }\n')
            child = root / 'gcp/proton_bridge.py'
            for said, status, expected in cases:
                with self.subTest(said=said[:40], status=status):
                    child.write_text('import sys\nsys.stderr.write(' + repr(said) + ')\nsys.exit(' + str(status) + ')\n')
                    result = subprocess.run(['/bin/bash', str(entry), 'capture'], capture_output=True)
                    self.assertEqual(result.returncode, 75)
                    self.assertEqual(result.stdout, b'')
                    self.assertEqual(result.stderr.decode(), expected)

    def test_root_capture_entrypoint_refuses_unprivileged_execution(self):
        p = unprivileged_capture()
        self.assertEqual(p.returncode, 75, p.stderr.decode())
        self.assertNotIn(SECRET.encode(), p.stdout + p.stderr)

    def test_exact_connected_configuration_has_one_shared_password(self):
        m = module()
        row = m.parse_configuration(configuration().encode(), ACCOUNT)
        self.assertEqual(row['password'], SECRET)
        self.assertEqual(row['imap_port'], 1143)
        self.assertEqual(row['smtp_port'], 1025)
        self.assertEqual(row['account'], ACCOUNT)

    def test_signed_out_empty_and_no_account_are_unavailable(self):
        m = module()
        for raw in (b'', b'No active accounts. Please add account to continue.\n',
                    f'Please login to {ACCOUNT} to get email client configuration.\n'.encode()):
            with self.subTest(raw=raw):
                with self.assertRaises(m.Refused):
                    m.parse_configuration(raw, ACCOUNT)

    def test_missing_second_and_foreign_account_configurations_refuse_differently(self):
        # 6e139256: live capture a05fe7af refused with one reason for three
        # situations that need three different fixes (owner re-login, a second
        # stale account, the CLI naming another account). The reason names the
        # count class and the match, never a value from the CLI output.
        m = module()
        one = configuration()
        cases = (
            (b'', 'no connected account configuration; the owner login is missing or signed out'),
            (b'No active accounts. Please add account to continue.\n',
             'no connected account configuration; the owner login is missing or signed out'),
            (f'Please login to {ACCOUNT} to get email client configuration.\n'.encode(),
             'no connected account configuration; the owner login is missing or signed out'),
            ((one + one).encode(),
             'more than one connected account configuration; a second account must be signed out'),
            ((one + one.replace(ACCOUNT, 'other@example.invalid')).encode(),
             'more than one connected account configuration; a second account must be signed out'),
            (one.replace(ACCOUNT, 'other@example.invalid').encode(),
             'the one connected account configuration names another account'),
        )
        for raw, reason in cases:
            with self.subTest(raw=raw[:40]):
                with self.assertRaises(m.Refused) as refused:
                    m.parse_configuration(raw, ACCOUNT)
                self.assertEqual(refused.exception.args, (reason,))
                self.assertIn(reason, m.SAFE_REFUSALS)
                self.assertNotIn('other@example.invalid', reason)
        self.assertNotIn('exactly one connected account configuration is required', m.SAFE_REFUSALS,
                         'the one-reason-for-three-situations constant is retired')

    def test_ambiguous_foreign_malformed_or_reordered_output_refuses(self):
        m = module()
        raw = configuration()
        bad = [raw + raw, raw.replace(ACCOUNT, 'other@example.invalid'),
               raw.replace('SMTP Settings', 'IMAP Settings'),
               raw.replace('1143', '0'), raw.replace('127.0.0.1', '0.0.0.0'),
               raw.replace('STARTTLS', 'cleartext'), raw.replace('Password:  '+SECRET, 'Password:  '),
               raw.replace('IMAP Settings\nAddress:', 'Address:\nIMAP Settings'),
               raw.replace('SMTP Settings', 'UNEXPECTED\nSMTP Settings'),
               raw.replace('Password:  '+SECRET, 'Password:  '+SECRET+'\x00'),
               raw.replace('SMTP port: 1025', 'SMTP port: nope')]
        for value in bad:
            with self.subTest(value=value):
                with self.assertRaises(m.Refused):
                    m.parse_configuration(value.encode(), ACCOUNT)

    def test_ansi_banner_and_runtime_messages_do_not_become_configuration(self):
        m = module()
        raw = b'\x1b[33mWARN\x1b[0m startup\nWelcome to Proton Mail Bridge interactive shell\n'
        self.assertEqual(m.parse_configuration(raw + configuration().encode(), ACCOUNT)['password'], SECRET)
        with self.assertRaises(m.Refused):
            m.parse_configuration(configuration().encode() + b'Password: injected\n', ACCOUNT)

    def test_two_different_protocol_passwords_are_not_one_fact(self):
        m = module()
        raw = configuration().rsplit(SECRET, 1)
        with self.assertRaises(m.Refused):
            m.parse_configuration(('different'.join(raw)).encode(), ACCOUNT)

    def test_atomic_private_file_and_preservation_on_refusal(self):
        m = module()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'private' / 'adapter.json'
            path.parent.mkdir(mode=0o700)
            m.write_credential(path, m.parse_configuration(configuration().encode(), ACCOUNT))
            old = path.read_bytes()
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            self.assertEqual(json.loads(old)['password'], SECRET)
            with self.assertRaises(m.Refused):
                m.parse_configuration(b'No active accounts.', ACCOUNT)
            self.assertEqual(path.read_bytes(), old)
            link = path.parent / 'linked'
            link.symlink_to(path)
            with self.assertRaises(m.Refused):
                m.write_credential(link, {'password': 'replacement'})
            self.assertEqual(path.read_bytes(), old)

    def test_world_writable_parent_is_not_a_credential_location(self):
        m = module()
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory);parent.chmod(0o777)
            with self.assertRaises(m.Refused):
                m.write_credential(parent / 'credential', {'password': SECRET})

    def test_capture_stops_verifies_gone_before_cli_and_restores_daemon(self):
        m = module();calls = []
        def service(action):
            calls.append(action)
            return 'inactive/dead/0' if action == 'state' else ''
        def cli():
            calls.append('cli')
            return configuration().encode()
        row = m.coordinated_capture(service, cli, ACCOUNT)
        self.assertEqual(row['password'], SECRET)
        self.assertEqual(calls, ['stop', 'state', 'cli', 'start'])

    def test_stop_unknown_never_launches_cli(self):
        m = module();calls = []
        def service(action):
            calls.append(action)
            return 'active/running/123' if action == 'state' else ''
        with self.assertRaises(m.Refused):
            m.coordinated_capture(service, lambda: self.fail('concurrent CLI'), ACCOUNT)
        self.assertNotIn('cli', calls)

    def test_timeout_or_bad_cli_preserves_file_and_restarts(self):
        m = module()
        for failure in ('timeout', 'malformed'):
            calls=[]
            def service(action):
                calls.append(action)
                return 'inactive/dead/0' if action == 'state' else ''
            def cli():
                if failure == 'timeout':
                    raise m.Refused('CLI deadline exceeded')
                return b'No active accounts.'
            with tempfile.TemporaryDirectory() as directory:
                path=Path(directory)/'secret';path.write_bytes(b'old');path.chmod(0o600)
                with self.assertRaises(m.Refused):
                    row=m.coordinated_capture(service, cli, ACCOUNT)
                    m.write_credential(path,row)
                self.assertEqual(path.read_bytes(),b'old')
                self.assertEqual(calls[-1],'start')

    def test_real_subprocess_fixed_stdin_and_bounded_output(self):
        m=module()
        with tempfile.TemporaryDirectory() as directory:
            child=Path(directory)/'child.py'
            # 34d95817: capture no longer has a fixed input; install's two
            # fixed inputs still pass whole and then close stdin.
            child.write_text('import sys\nassert sys.stdin.buffer.read()==b"updates autoupdates disable\\nexit\\n"\nprint("No active accounts.")\n')
            raw=m.private_cli([sys.executable,str(child)],dict(os.environ),3,data=b'updates autoupdates disable\nexit\n')
            self.assertIn(b'No active accounts.',raw)
            child.write_text('import sys\nsys.stdin.buffer.read()\nsys.stdout.write("x"*131073)\n')
            with self.assertRaises(m.Refused):
                m.private_cli([sys.executable,str(child)],dict(os.environ),3)
            child.write_text('import time\ntime.sleep(20)\n')
            with self.assertRaises(m.Refused):
                m.private_cli([sys.executable,str(child)],dict(os.environ),0.1)

    def test_repeated_converge_does_not_stop_a_logged_in_daemon(self):
        m=module();calls=[];downloads=[];configurations=[]
        # Privileged package/account/service effects are a port fixture. The
        # actual file bytes, permissions and repeated install run unchanged.
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);systemd=root/'systemd';systemd.mkdir()
            declaration=m.pin();declaration['executables']={'proton-bridge':__import__('hashlib').sha256(b'fixture').hexdigest(),'bridge':__import__('hashlib').sha256(b'fixture').hexdigest()}
            def fetched(url,expected,destination,limit):
                downloads.append(url);destination.write_bytes(b'fixture')
            def checked(argv,**kwargs):
                calls.append(argv)
                if argv[:2]==['/usr/bin/dpkg-deb','--extract']:
                    target=Path(argv[-1])/'usr/lib/protonmail/bridge';target.mkdir(parents=True)
                    for name in ('proton-bridge','bridge'):(target/name).write_bytes(b'fixture')
                return b''
            def service(action):
                calls.append(['service',action]);return 'inactive/dead/0'
            def private_cli(argv,env,**kwargs):
                configurations.append(kwargs.get('data'))
                return b'Bridge is already set to NOT automatically install updates.'
            with patch.multiple(m,STORE=root/'store',STATE=root/'state',PRIVATE=root/'private',SYSTEMD=systemd),patch.object(m,'pin',return_value=declaration),patch.object(m,'fetch_verified',side_effect=fetched),patch.object(m,'checked',side_effect=checked),patch.object(m,'service',side_effect=service),patch.object(m,'private_cli',side_effect=private_cli),patch.object(m.pwd,'getpwnam',return_value=SimpleNamespace(pw_uid=os.getuid(),pw_gid=os.getgid())):
                (root/'state/password-store').mkdir(parents=True);(root/'state/password-store/.gpg-id').write_text('fixture')
                m.install()
                calls.clear()
                m.install()
                self.assertEqual(len(downloads),3)
                self.assertEqual(configurations,[b'updates autoupdates disable\nyes\nexit\n',b'updates autoupdates disable\nexit\n'],
                                 'confirm the setting then read it back once; do not reopen on later converge')
                self.assertNotIn(['service','stop'],calls)
                unit=(systemd/m.UNIT).read_text()
                self.assertIn('User=boss-bridge',unit)
                self.assertNotIn(' capture',unit)
                self.assertNotIn('@CONTROL@',unit)

    def test_directory_sync_failure_leaves_capture_outcome_unconfirmed(self):
        m=module();real_sync=os.fsync
        with tempfile.TemporaryDirectory() as directory:
            target=Path(directory)/'credential';target.write_bytes(b'old');target.chmod(0o600)
            def sync(fd):
                if stat.S_ISDIR(os.fstat(fd).st_mode):
                    raise OSError('injected directory sync failure after replacement')
                return real_sync(fd)
            with patch.object(m.os,'fsync',side_effect=sync):
                with self.assertRaises(OSError):m.write_credential(target,{'dummy':True})
            # A failed acknowledgement does not undo the rename. The common
            # failure diagnostic must not promise that the old bytes survived.
            self.assertEqual(json.loads(target.read_bytes()),{'dummy':True})
            result=unprivileged_capture()
            self.assertEqual(result.returncode,75)
            self.assertIn(b'outcome unconfirmed',result.stderr)
            self.assertNotIn(b'unchanged',result.stderr.lower())


if __name__ == '__main__':
    unittest.main()
