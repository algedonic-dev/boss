#!/usr/bin/env python3
"""67e08c6a: a local Bridge session, never a mailbox-password transport.

The owner logs in interactively. Capture uses the same stopped service's vault;
its output is private input, never an ops-request's diagnostic output.
"""
import contextlib
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import urllib.request

ACCOUNT = 'agent-b@algedonic.dev'
USER = 'boss-bridge'
UNIT = 'boss-proton-bridge.service'
STATE = Path('/var/lib/boss-bridge')
PRIVATE = Path('/etc/boss-bridge')
STORE = Path('/opt/boss-bridge')
SYSTEMD = Path('/etc/systemd/system')
LOCK_DIR = Path('/run/boss-bridge')
LIMIT = 128 * 1024
INPUT = b'info agent-b@algedonic.dev\nexit\n'
CLI_SECONDS = 45
CLI_CLEANUP_SECONDS = 5
SERVICE_STATE_SECONDS = 5
SERVICE_CHANGE_SECONDS = 20


# Only these authored constants may cross the diagnostic boundary. Never print
# str(error): even a Refused supplied by a future caller may contain CLI input.
SAFE_REFUSALS = frozenset({
    'Bridge login, capture or converge already owns the session',
    'Bridge state is linked or unavailable',
    'CLI cleanup is unproven',
    'CLI deadline exceeded; credential unchanged',
    'CLI failed; credential unchanged',
    'CLI output bound exceeded; credential unchanged',
    'CLI termination is unproven',
    'capture directory is not root-private',
    'configuration contains ambiguous fields',
    'configuration encoding is unavailable',
    'configuration fields are incomplete',
    'configuration fields contradict the local account',
    'configuration is oversized or malformed',
    'configuration ports are invalid',
    'configuration ports are unavailable',
    'configuration receipt is not root-private',
    'configuration sections are malformed',
    'configuration transport security is unsupported',
    'credential destination is linked',
    'credential parent must be private and owned',
    'daemon requires its dedicated non-root identity',
    'daemon termination is unproven',
    'daemon termination is unproven; no second vault process started',
    'existing credential is not an owned private file',
    'expected fixed install, capture, login or daemon operation',
    'installed control bytes differ from their identity',
    'installed release bytes differ from the pin',
    'more than one connected account configuration; a second account must be signed out',
    'no connected account configuration; the owner login is missing or signed out',
    'one printable Bridge password must cover both protocols',
    'owner login did not finish',
    'owner login requires a private interactive terminal',
    'pinned update policy readback is unavailable',
    'release download does not match its declared hash',
    'release download exceeds its bound',
    'release store is not root-protected',
    'required local operation did not finish',
    'required local operation refused; no diagnostic values copied',
    'root-only Bridge operation',
    'singleton lock is not root-owned and private',
    'singleton parent is not owned and private',
    'the one connected account configuration names another account',
})


class Refused(Exception):
    """A bounded diagnostic, never the CLI's secret-bearing output."""


def parse_configuration(raw, account):
    if len(raw) > LIMIT or b'\x00' in raw:
        raise Refused('configuration is oversized or malformed')
    try:
        text = raw.decode('utf-8')
    except UnicodeError as error:
        raise Refused('configuration encoding is unavailable') from error
    text = re.sub(r'\x1b\[[0-9;]*m', '', text)
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    starts = [i for i, line in enumerate(lines) if line.startswith('Configuration for ')]
    # Three situations, three fixes (backlog 6e139256): no block means the
    # owner login is missing or signed out; a second block is a stale account
    # to sign out; one block for another account is the CLI naming a different
    # header. Each reason is a count class and a match, never a CLI value.
    if not starts:
        raise Refused('no connected account configuration; the owner login is missing or signed out')
    if len(starts) > 1:
        raise Refused('more than one connected account configuration; a second account must be signed out')
    if lines[starts[0]] != 'Configuration for '+account:
        raise Refused('the one connected account configuration names another account')
    start = starts[0]
    expected = [('IMAP Settings', None), ('Address', '127.0.0.1'), ('IMAP port', None),
                ('Username', account), ('Password', None), ('Security', None),
                ('SMTP Settings', None), ('Address', '127.0.0.1'), ('SMTP port', None),
                ('Username', account), ('Password', None), ('Security', None)]
    block = lines[start+1:start+13]
    if len(block) != len(expected):
        raise Refused('configuration fields are incomplete')
    values = []
    for line, (name, exact) in zip(block, expected):
        if name.endswith(' Settings'):
            if line != name:
                raise Refused('configuration sections are malformed')
            values.append(None)
            continue
        key, separator, value = line.partition(':')
        value = value.strip()
        if separator != ':' or key != name or not value or (exact and value != exact):
            raise Refused('configuration fields contradict the local account')
        values.append(value)
    # Banners and asynchronous diagnostics may surround the ONE block, but a
    # second field/section cannot be smuggled into the ignored output.
    outside = lines[:start] + lines[start+13:]
    if any(re.match(r'^(Configuration for |(?:IMAP|SMTP) Settings|(?:Address|Username|Password|Security|IMAP port|SMTP port):)', line) for line in outside):
        raise Refused('configuration contains ambiguous fields')
    try:
        imap, smtp = int(values[2]), int(values[8])
    except ValueError as error:
        raise Refused('configuration ports are unavailable') from error
    if not (0 < imap < 65536 and 0 < smtp < 65536) or imap == smtp:
        raise Refused('configuration ports are invalid')
    if values[5] not in ('STARTTLS', 'SSL') or values[11] not in ('STARTTLS', 'SSL'):
        raise Refused('configuration transport security is unsupported')
    if values[4] != values[10] or any(ord(c) < 33 or ord(c) > 126 for c in values[4]):
        raise Refused('one printable Bridge password must cover both protocols')
    return {'schema': 1, 'account': account, 'password': values[4],
            'host': '127.0.0.1', 'imap_port': imap, 'smtp_port': smtp,
            'imap_security': values[5], 'smtp_security': values[11]}


def write_credential(path, value):
    parent = path.parent
    info = parent.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o077:
        raise Refused('credential parent must be private and owned')
    if path.is_symlink():
        raise Refused('credential destination is linked')
    if path.exists():
        old = path.lstat()
        if not stat.S_ISREG(old.st_mode) or old.st_uid != os.geteuid() or stat.S_IMODE(old.st_mode) != 0o600:
            raise Refused('existing credential is not an owned private file')
    fd, temporary = tempfile.mkstemp(prefix='.capture-', dir=parent)
    try:
        with os.fdopen(fd, 'wb') as stream:
            os.fchmod(stream.fileno(), 0o600)
            stream.write((json.dumps(value, sort_keys=True)+'\n').encode())
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def private_cli(argv, env, seconds=CLI_SECONDS, data=INPUT):
    process = subprocess.Popen(argv, env=env, stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               start_new_session=True)
    output = {process.stdout: bytearray(), process.stderr: bytearray()}
    deadline = time.monotonic()+seconds
    try:
        process.stdin.write(data)
        process.stdin.close()
        with selectors.DefaultSelector() as selector:
            for pipe in output:
                selector.register(pipe, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline-time.monotonic()
                if remaining <= 0:
                    raise Refused('CLI deadline exceeded; credential unchanged')
                for key, _ in selector.select(min(remaining, 0.1)):
                    chunk = os.read(key.fileobj.fileno(), 8192)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    output[key.fileobj].extend(chunk)
                    if sum(map(len, output.values())) > LIMIT:
                        raise Refused('CLI output bound exceeded; credential unchanged')
        try:
            status = process.wait(timeout=max(0.01, deadline-time.monotonic()))
        except subprocess.TimeoutExpired as error:
            raise Refused('CLI termination is unproven') from error
        if status != 0 or output[process.stderr]:
            raise Refused('CLI failed; credential unchanged')
        return bytes(output[process.stdout])
    finally:
        # The session leader can exit while a descendant still owns the pipes.
        # Reap the whole owned session on both timeout and normal completion.
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGKILL)
        try:
            process.wait(timeout=CLI_CLEANUP_SECONDS)
        except subprocess.TimeoutExpired as error:
            raise Refused('CLI cleanup is unproven') from error
        finally:
            process.stdout.close()
            process.stderr.close()


def coordinated_capture(service, cli, account):
    try:
        service('stop')
        if service('state') != 'inactive/dead/0':
            raise Refused('daemon termination is unproven; no second vault process started')
        return parse_configuration(cli(), account)
    finally:
        # A failed capture restores availability but never claims a saved value.
        service('start')


def checked(argv, env=None, seconds=45):
    try:
        result = subprocess.run(argv, env=env, capture_output=True, timeout=seconds)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise Refused('required local operation did not finish') from error
    if result.returncode:
        raise Refused('required local operation refused; no diagnostic values copied')
    return result.stdout


def service(action):
    if action == 'state':
        values = checked(['/usr/bin/systemctl', 'show', UNIT, '--property=ActiveState,SubState,MainPID'], seconds=SERVICE_STATE_SECONDS).decode().splitlines()
        fields = dict(line.split('=', 1) for line in values)
        return '/'.join(fields.get(key, 'unknown') for key in ('ActiveState', 'SubState', 'MainPID'))
    checked(['/usr/bin/systemctl', action, UNIT], seconds=SERVICE_CHANGE_SECONDS)
    return ''


def environment():
    return {'PATH': '/usr/bin:/bin', 'HOME': str(STATE),
            'GNUPGHOME': str(STATE/'gnupg'), 'PASSWORD_STORE_DIR': str(STATE/'password-store'),
            'XDG_CONFIG_HOME': str(STATE/'config'), 'XDG_DATA_HOME': str(STATE/'data'),
            'XDG_CACHE_HOME': str(STATE/'cache'), 'LC_ALL': 'C.UTF-8', 'TZ': 'UTC'}


def user_argv(argv):
    return ['/usr/sbin/runuser', '-u', USER, '--', '/usr/bin/env', '-i',
            *[key+'='+value for key, value in environment().items()], *argv]


def pin():
    return json.loads(Path(__file__).with_name('proton-bridge.json').read_text())


def executable():
    return str(STORE/pin()['version']/'usr/lib/protonmail/bridge/proton-bridge')


@contextlib.contextmanager
def singleton():
    LOCK_DIR.mkdir(mode=0o700, exist_ok=True)
    parent = LOCK_DIR.lstat()
    if not stat.S_ISDIR(parent.st_mode) or parent.st_uid != os.geteuid() or stat.S_IMODE(parent.st_mode) != 0o700:
        raise Refused('singleton parent is not owned and private')
    fd = os.open(LOCK_DIR/'session.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(fd)
        if info.st_uid != os.geteuid() or not stat.S_ISREG(info.st_mode) or stat.S_IMODE(info.st_mode) != 0o600:
            raise Refused('singleton lock is not root-owned and private')
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise Refused('Bridge login, capture or converge already owns the session') from error
        yield
    finally:
        os.close(fd)


def fetch_verified(url, expected, destination, limit):
    digest = hashlib.sha256()
    with urllib.request.urlopen(url, timeout=60) as response, destination.open('xb') as output:
        count = 0
        while chunk := response.read(1024*1024):
            count += len(chunk)
            if count > limit:
                raise Refused('release download exceeds its bound')
            digest.update(chunk)
            output.write(chunk)
    if digest.hexdigest() != expected:
        raise Refused('release download does not match its declared hash')


def install():
    declaration = pin()
    STORE.mkdir(mode=0o755, exist_ok=True)
    if STORE.is_symlink() or STORE.stat().st_uid != os.geteuid() or STORE.stat().st_mode & 0o022:
        raise Refused('release store is not root-protected')
    release = STORE/declaration['version']
    if not release.exists():
        with tempfile.TemporaryDirectory(prefix='.bridge-release-', dir=STORE) as directory:
            stage = Path(directory)
            for name, artifact in declaration['artifacts'].items():
                fetch_verified(artifact['url'], artifact['sha256'], stage/name, artifact['max_bytes'])
            checked(['/usr/bin/gpg', '--batch', '--dearmor', '--output', str(stage/'keyring'), str(stage/'key.asc')])
            checked(['/usr/bin/gpgv', '--keyring', str(stage/'keyring'), str(stage/'package.sig'), str(stage/'package.deb')])
            checked(['/usr/bin/dpkg-deb', '--extract', str(stage/'package.deb'), str(stage/'unpacked')])
            os.rename(stage/'unpacked', release)
    for name, expected in declaration['executables'].items():
        path = release/'usr/lib/protonmail/bridge'/name
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022 or hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            raise Refused('installed release bytes differ from the pin')
    # Dependencies come from the host's signed distro repositories, not a curl
    # installer. Package scripts for Bridge itself are never executed as root.
    checked(['/usr/bin/apt-get', 'update'], seconds=180)
    checked(['/usr/bin/apt-get', 'install', '-y', 'libsecret-1-0', 'pass', 'libfido2-1'], seconds=180)
    try:
        account = pwd.getpwnam(USER)
    except KeyError:
        checked(['/usr/sbin/useradd', '--system', '--home-dir', str(STATE), '--shell', '/usr/sbin/nologin', USER])
        account = pwd.getpwnam(USER)
    STATE.mkdir(mode=0o700, exist_ok=True)
    if STATE.is_symlink() or not STATE.is_dir():
        raise Refused('Bridge state is linked or unavailable')
    os.chown(STATE, account.pw_uid, account.pw_gid)
    os.chmod(STATE, 0o700)
    PRIVATE.mkdir(mode=0o700, exist_ok=True)
    if PRIVATE.is_symlink() or PRIVATE.stat().st_uid != os.geteuid() or PRIVATE.stat().st_mode & 0o077:
        raise Refused('capture directory is not root-private')
    if not (STATE/'password-store/.gpg-id').exists():
        checked(user_argv(['/usr/bin/mkdir', '-p', str(STATE/'gnupg')]))
        checked(user_argv(['/usr/bin/chmod', '0700', str(STATE/'gnupg')]))
        checked(user_argv(['/usr/bin/gpg', '--batch', '--pinentry-mode', 'loopback', '--passphrase', '',
                           '--quick-generate-key', 'BOSS Bridge local keychain', 'default', 'default', 'never']))
        checked(user_argv(['/usr/bin/pass', 'init', 'BOSS Bridge local keychain']))
    # A reviewed version must not quietly become a newer user-space update.
    service_present = (SYSTEMD/UNIT).exists()
    configured = PRIVATE/('configured-'+declaration['version']+'.json')
    if configured.exists() and (configured.is_symlink() or configured.stat().st_uid != os.geteuid()
                                or configured.stat().st_mode & 0o077):
        raise Refused('configuration receipt is not root-private')
    if not configured.exists():
        if service_present:
            service('stop')
            if service('state') != 'inactive/dead/0':
                raise Refused('daemon termination is unproven')
        try:
            private_cli(user_argv([executable(), '--cli']), {'PATH': '/usr/bin:/bin'},
                        data=b'updates autoupdates disable\nyes\nexit\n')
            observed = private_cli(user_argv([executable(), '--cli']), {'PATH': '/usr/bin:/bin'},
                                   data=b'updates autoupdates disable\nexit\n')
            if b'Bridge is already set to NOT automatically install updates.' not in observed:
                raise Refused('pinned update policy readback is unavailable')
            write_credential(configured, {'version': declaration['version']})
        finally:
            if service_present:
                service('start')
    # An immutable, root-owned copy avoids holding a source-generation lock for
    # the lifetime of a daemon, which would prevent the next converge entirely.
    own = Path(__file__)
    data = own.read_bytes()+own.with_name('proton-bridge.json').read_bytes()
    control = STORE/'control'/hashlib.sha256(data).hexdigest()
    control.mkdir(mode=0o755, parents=True, exist_ok=True)
    for source in (own, own.with_name('proton-bridge.json')):
        target = control/source.name
        if target.exists() and target.read_bytes() != source.read_bytes():
            raise Refused('installed control bytes differ from their identity')
        if not target.exists():
            target.write_bytes(source.read_bytes())
            target.chmod(0o644)
    template = own.with_name('proton-bridge.service').read_text()
    unit = template.replace('@CONTROL@', str(control/own.name))
    unit_path = SYSTEMD/UNIT
    changed = not unit_path.exists() or unit_path.read_text() != unit
    if changed:
        if service_present:
            service('stop')
        unit_path.write_text(unit)
        checked(['/usr/bin/systemctl', 'daemon-reload'])
    checked(['/usr/bin/systemctl', 'enable', '--now', UNIT])
    print('Bridge '+declaration['version']+' installed; owner login and real capture UNPAID')


def main():
    if len(sys.argv) == 2 and sys.argv[1] == 'daemon':
        if os.geteuid() == 0 or os.geteuid() != pwd.getpwnam(USER).pw_uid:
            raise Refused('daemon requires its dedicated non-root identity')
        os.execve(executable(), [executable(), '--noninteractive'], environment())
    if os.geteuid() != 0:
        raise Refused('root-only Bridge operation')
    if len(sys.argv) != 2 or sys.argv[1] not in ('install', 'capture', 'login', 'daemon'):
        raise Refused('expected fixed install, capture, login or daemon operation')
    action = sys.argv[1]
    with singleton():
        if action == 'install':
            install()
        elif action == 'capture':
            row = coordinated_capture(service, lambda: private_cli(user_argv([executable(), '--cli']), {'PATH': '/usr/bin:/bin'}), ACCOUNT)
            row['captured_at'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
            row['bridge_version'] = pin()['version']
            target = PRIVATE/'adapter-credential.json'
            write_credential(target, row)
            print('path='+str(target)+' bytes='+str(target.stat().st_size))
        else:
            if not sys.stdin.isatty() or not sys.stdout.isatty():
                raise Refused('owner login requires a private interactive terminal')
            service('stop')
            try:
                if service('state') != 'inactive/dead/0':
                    raise Refused('daemon termination is unproven')
                status = subprocess.run(user_argv([executable(), '--cli']), check=False).returncode
                if status:
                    raise Refused('owner login did not finish')
            finally:
                service('start')


if __name__ == '__main__':
    try:
        main()
    except Refused as error:
        # Select the stored constant, not the exception's printable value.
        reason = next((text for text in SAFE_REFUSALS if type(error.args) is tuple
                       and len(error.args) == 1 and type(error.args[0]) is str
                       and error.args[0] == text), None)
        if reason is None:
            print('Bridge operation UNAVAILABLE; credential file outcome unconfirmed', file=sys.stderr)
        else:
            print('Bridge operation UNAVAILABLE; Refused: ' + reason + '; credential file outcome unconfirmed', file=sys.stderr)
        sys.exit(75)
    except (OSError, ValueError, KeyError, subprocess.SubprocessError):
        # Never stringify a CLI/parser exception: its input may be a password.
        print('Bridge operation UNAVAILABLE; credential file outcome unconfirmed', file=sys.stderr)
        sys.exit(75)
